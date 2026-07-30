//! Browser-based SSO — RFC 8252 native-app flow with PKCE (ADR-0009). Using the **system browser** is what
//! makes passkeys / WebAuthn, MFA, and enterprise SAML work: the OS + browser run the ceremony, and we only
//! receive the resulting token via a `127.0.0.1` loopback redirect.
//!
//! ⚠️ **This OAuth path is NOT the flow a consumer UniFi Identity account uses — see `TODO.md`.** The
//! endpoints below are real (`sso.ui.com`'s OIDC discovery at `/oauth2/.well-known/openid-configuration`
//! confirms a Django-OAuth-Toolkit server: `authorization_endpoint = /oauth2/authorize`,
//! `token_endpoint = /oauth2/token`, PKCE `S256`, `scopes_supported = [read, billing, openid, introspection,
//! ui]`), but that DOT server backs the **enterprise "SSO Apps"** feature and needs a registered `client_id`
//! we don't have (it rejects any we try). The **real** onboarding for this account is an **invite →
//! device-credential → UCS `vpn/session`** flow (`identity-standard://` deep link → `enterprise.svc.ui.com`);
//! that's the target — implement it in [`crate::ucs`], not here. This module is kept because its **PKCE +
//! loopback + code→token mechanism is generic and unit-tested**, and reusable if a real OAuth client ever
//! appears. Do not wire `AuthConfig`/`run_login_flow` into the product path without confirming a valid client.

use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::{Error, Result};

const B64URL: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// PKCE verifier/challenge pair (RFC 7636, S256).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generate from 32 bytes of OS randomness.
    pub fn generate() -> Self {
        use rand_core::RngCore;
        let mut bytes = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut bytes);
        Self::from_verifier(B64URL.encode(bytes))
    }

    /// Build the challenge for a given verifier: `base64url(sha256(verifier))`.
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
        Pkce { verifier, challenge }
    }
}

/// A random opaque value for the OAuth `state` CSRF check.
pub fn random_state() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut bytes);
    B64URL.encode(bytes)
}

/// SSO endpoint configuration. Defaults are best-guesses to be confirmed by capture (M7).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            // Confirmed from https://sso.ui.com/oauth2/.well-known/openid-configuration (see module docs).
            authorize_url: "https://sso.ui.com/oauth2/authorize".into(),
            token_url: "https://sso.ui.com/oauth2/token".into(),
            // UNCONFIRMED: the desktop app's registered public client id (DOT validates it only post-login,
            // so it can't be probed anonymously). Pin from the real UniFi Endpoint app / an M7 capture.
            client_id: "unifi-endpoint".into(),
            // `openid` is valid; `profile` is NOT in scopes_supported. Widen (e.g. `openid ui`) once we know
            // what the UCS API requires.
            scopes: "openid".into(),
        }
    }
}

/// Build the browser authorize URL for the PKCE flow.
pub fn authorize_url(cfg: &AuthConfig, pkce: &Pkce, state: &str, redirect_uri: &str) -> String {
    let mut url = url::Url::parse(&cfg.authorize_url).expect("authorize_url is a valid URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &cfg.scopes)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

/// A one-shot loopback HTTP endpoint that captures the OAuth redirect.
pub struct Loopback {
    listener: TcpListener,
    /// The `http://127.0.0.1:<port>/callback` URL to register as the redirect.
    pub redirect_uri: String,
}

impl Loopback {
    /// Bind `127.0.0.1` on an ephemeral port.
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(io)?;
        let port = listener.local_addr().map_err(io)?.port();
        Ok(Loopback { listener, redirect_uri: format!("http://127.0.0.1:{port}/callback") })
    }

    /// Accept the browser redirect and return `(code, state)` from its query string.
    pub async fn wait_for_code(self) -> Result<(String, String)> {
        let (mut stream, _) = self.listener.accept().await.map_err(io)?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.map_err(io)?;
        let request = String::from_utf8_lossy(&buf[..n]);
        // Request line: `GET /callback?code=...&state=... HTTP/1.1`
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");
        let (code, state) = parse_callback(target);

        let body = "<html><body style=\"font-family:sans-serif\">Signed in — you can close this window.</body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        match (code, state) {
            (Some(c), Some(s)) => Ok((c, s)),
            _ => Err(Error::Other(anyhow::anyhow!("authorization redirect had no code"))),
        }
    }
}

fn parse_callback(target: &str) -> (Option<String>, Option<String>) {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let (mut code, mut state) = (None, None);
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    (code, state)
}

/// Token endpoint response (only the fields we use).
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Exchange an authorization code for a token (PKCE, no client secret).
pub async fn exchange_code(
    cfg: &AuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("client_id", &cfg.client_id),
    ];
    let resp = reqwest::Client::new()
        .post(&cfg.token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Http(format!("token endpoint returned HTTP {}", resp.status().as_u16())));
    }
    resp.json::<TokenResponse>().await.map_err(|e| Error::Http(e.to_string()))
}

/// Open a URL in the user's default browser.
pub fn open_browser(url: &str) -> Result<()> {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(program)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| Error::Other(anyhow::anyhow!("couldn't open the browser: {e}")))
}

/// Run the whole browser SSO flow and return the access token: generate PKCE + state, bind a loopback
/// redirect, open the browser, wait for the code (verifying `state`), and exchange it for a token.
pub async fn run_login_flow(cfg: &AuthConfig) -> Result<String> {
    let pkce = Pkce::generate();
    let state = random_state();
    let loopback = Loopback::bind().await?;
    let url = authorize_url(cfg, &pkce, &state, &loopback.redirect_uri);
    let redirect_uri = loopback.redirect_uri.clone();
    open_browser(&url)?;
    let (code, returned_state) = loopback.wait_for_code().await?;
    if returned_state != state {
        return Err(Error::Other(anyhow::anyhow!("state mismatch (possible CSRF)")));
    }
    let token = exchange_code(cfg, &code, &pkce.verifier, &redirect_uri).await?;
    Ok(token.access_token)
}

fn io(e: std::io::Error) -> Error {
    Error::Other(anyhow::anyhow!("loopback: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_test_vector() {
        // RFC 7636 Appendix B.
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string());
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn authorize_url_carries_pkce_params() {
        let cfg = AuthConfig::default();
        let pkce = Pkce::from_verifier("verifier".into());
        let url = authorize_url(&cfg, &pkce, "st4te", "http://127.0.0.1:9/callback");
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback"));
    }

    #[tokio::test]
    async fn loopback_captures_code_and_state() {
        let loopback = Loopback::bind().await.unwrap();
        let addr = loopback.redirect_uri.clone();
        // Extract host:port from the redirect URI.
        let hostport = addr.trim_start_matches("http://").trim_end_matches("/callback").to_string();

        let server = tokio::spawn(async move { loopback.wait_for_code().await });

        // Simulate the browser hitting the redirect.
        let mut client = tokio::net::TcpStream::connect(&hostport).await.unwrap();
        client
            .write_all(b"GET /callback?code=the-code&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        let (code, state) = server.await.unwrap().unwrap();
        assert_eq!(code, "the-code");
        assert_eq!(state, "xyz");
        assert!(String::from_utf8_lossy(&resp).contains("close this window"));
    }

    #[tokio::test]
    async fn exchange_code_parses_token() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "the-jwt", "token_type": "Bearer", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let cfg = AuthConfig {
            authorize_url: "https://x/authorize".into(),
            token_url: format!("{}/token", server.uri()),
            client_id: "cid".into(),
            scopes: "openid".into(),
        };
        let token = exchange_code(&cfg, "code", "verifier", "http://127.0.0.1/callback").await.unwrap();
        assert_eq!(token.access_token, "the-jwt");
        assert_eq!(token.expires_in, Some(3600));
    }
}
