//! Client for the UniFi Identity "UCS" API — the endpoints the desktop client actually uses (see
//! `docs/02-vpn-protocol-and-reference-clients.md`, "UPDATE"). Auth is a Bearer JWT obtained via browser SSO
//! (ADR-0009). Paths + field names are HIGH-confidence (static recon of the macOS binary); exact request
//! bodies are MEDIUM — a single traffic capture on the Linux box will confirm and let us tighten the structs.
//! Endpoints are overridable (constructor) so tests point them at a mock server.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::model::{Drive, Host, Identity, VpnSession};
use crate::{Error, Result};

/// Base hosts for the service. Defaults to production; tests/staging override.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// SSO/login host, e.g. `https://sso.ui.com`.
    pub sso: String,
    /// UID API gateway that fronts the `/proxy/ucs/...` + `/proxy/users/...` routes.
    pub api_gw: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            sso: "https://sso.ui.com".to_string(),
            api_gw: "https://api-gw.uid.df.ui.com".to_string(),
        }
    }
}

/// Thin, typed client over the UCS API.
pub struct UcsClient {
    http: reqwest::Client,
    endpoints: Endpoints,
    bearer: Option<String>,
}

impl UcsClient {
    pub fn new(endpoints: Endpoints) -> Self {
        Self { http: reqwest::Client::new(), endpoints, bearer: None }
    }

    /// Attach the SSO bearer token used for the `/proxy/...` calls.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    /// Replace the bearer token (after a fresh sign-in / reauth) or clear it (sign-out).
    pub fn set_token(&mut self, token: Option<String>) {
        self.bearer = token;
    }

    // ---- endpoints (paths verbatim from the macOS binary) ----

    /// The signed-in identity: `GET /proxy/users/public/api/v2/identity/info`.
    pub async fn identity(&self) -> Result<Identity> {
        self.get_json(&format!("{}/proxy/users/public/api/v2/identity/info", self.endpoints.api_gw))
            .await
    }

    /// Consoles/sites available to the user: `GET /user-token/hosts/`.
    pub async fn hosts(&self) -> Result<Vec<Host>> {
        self.get_json(&format!("{}/user-token/hosts/", self.endpoints.api_gw)).await
    }

    /// Enroll this device's WireGuard public key: `POST /proxy/users/public/api/v2/identity/public_key`.
    pub async fn enroll_public_key(&self, public_key: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(rename = "publicKey")]
            public_key: &'a str,
        }
        let url = format!("{}/proxy/users/public/api/v2/identity/public_key", self.endpoints.api_gw);
        self.post_no_content(&url, &Body { public_key }).await
    }

    /// Provision a VPN session for a console: `POST /proxy/ucs/public/user/api/v1/vpn/session`.
    /// Returns a standard WireGuard peer config (`wgConfig`).
    pub async fn create_vpn_session(&self, console_id: &str, public_key: &str) -> Result<VpnSession> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(rename = "consoleId")]
            console_id: &'a str,
            #[serde(rename = "publicKey")]
            public_key: &'a str,
        }
        let url = format!("{}/proxy/ucs/public/user/api/v1/vpn/session", self.endpoints.api_gw);
        self.post_json(&url, &Body { console_id, public_key }).await
    }

    /// List the user's UniFi Drive shares for a console.
    ///
    /// **UNCONFIRMED PATH.** The macOS binary exposes drive UI + `credential/import` routes but no clean
    /// "list drives" endpoint was captured in static recon; this path is a best guess to be verified by a
    /// traffic capture on the Linux box (see docs/02). The engine treats a failure here as "no drives yet",
    /// so an eventual path correction is low-risk.
    pub async fn drives(&self, console_id: &str) -> Result<Vec<Drive>> {
        let url = format!(
            "{}/proxy/ucs/public/user/api/v1/drive/list?consoleId={}",
            self.endpoints.api_gw, console_id
        );
        self.get_json(&url).await
    }

    // ---- transport helpers ----

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self.authed(self.http.get(url)).send().await.map_err(http_err)?;
        check_status(&resp)?;
        resp.json::<T>().await.map_err(http_err)
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(&self, url: &str, body: &B) -> Result<T> {
        let resp = self.authed(self.http.post(url).json(body)).send().await.map_err(http_err)?;
        check_status(&resp)?;
        resp.json::<T>().await.map_err(http_err)
    }

    async fn post_no_content<B: Serialize>(&self, url: &str, body: &B) -> Result<()> {
        let resp = self.authed(self.http.post(url).json(body)).send().await.map_err(http_err)?;
        check_status(&resp)
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.bearer {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }
}

fn http_err(e: reqwest::Error) -> Error {
    Error::Http(e.to_string())
}

/// Map HTTP status to a domain error so callers get an actionable [`Error`], not a raw code.
fn check_status(resp: &reqwest::Response) -> Result<()> {
    use reqwest::StatusCode;
    let s = resp.status();
    if s.is_success() {
        return Ok(());
    }
    match s {
        StatusCode::UNAUTHORIZED => Err(Error::SessionExpired),
        StatusCode::FORBIDDEN => Err(Error::AccountRestricted("access denied".to_string())),
        StatusCode::NOT_FOUND => Err(Error::NoConsoleAvailable),
        _ => Err(Error::Http(format!("HTTP {}", s.as_u16()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer) -> UcsClient {
        UcsClient::new(Endpoints { sso: "https://sso.ui.com".into(), api_gw: server.uri() })
            .with_token("test-jwt")
    }

    #[tokio::test]
    async fn parses_hosts_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user-token/hosts/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"consoleStandardId": "c1", "hostName": "Home"},
                {"consoleStandardId": "c2", "hostName": "Office", "wanIp": "203.0.113.7"}
            ])))
            .mount(&server)
            .await;

        let hosts = client_for(&server).hosts().await.unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].console_id, "c1");
        assert_eq!(hosts[0].name, "Home");
        assert_eq!(hosts[1].wan_ip.as_deref(), Some("203.0.113.7"));
    }

    #[tokio::test]
    async fn create_vpn_session_returns_wg_config() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/proxy/ucs/public/user/api/v1/vpn/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sessionId": "sess-1",
                "wgConfig": {
                    "serverPublicKey": "srv",
                    "endpoint": "203.0.113.7:51820",
                    "allowedIps": ["10.0.0.0/8"],
                    "persistentKeepalive": 25,
                    "clientAddress": ["10.2.0.9/32"]
                }
            })))
            .mount(&server)
            .await;

        let s = client_for(&server).create_vpn_session("c1", "mypubkey").await.unwrap();
        assert_eq!(s.session_id.as_deref(), Some("sess-1"));
        assert_eq!(s.wg.endpoint, "203.0.113.7:51820");
        assert!(s.wg.has_dialable_endpoint());
    }

    #[tokio::test]
    async fn unauthorized_maps_to_session_expired() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxy/users/public/api/v2/identity/info"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client_for(&server).identity().await.unwrap_err();
        assert!(matches!(err, Error::SessionExpired));
        // And it renders as a sign-in prompt, not a raw 401.
        assert_eq!(err.user_facing().action, crate::UserAction::SignIn);
    }
}
