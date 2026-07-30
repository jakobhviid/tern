//! UniFi Teleport client (ADR-0016) — the path that actually connects for a consumer account.
//!
//! Onboarding is a **Teleport invite** the console generates (Settings → VPN → Teleport), of the form
//! `https://teleport.ui.link/<uuid>`. On desktop that link is just a Firebase Dynamic Link to the WiFiman
//! mobile app (no custom scheme to register), so the user **pastes it** (or its bare UUID) and we act on it —
//! `Invite::parse` below turns either into a validated invite id.
//!
//! This module is a clean-room Rust port of the reverse-engineered Teleport protocol (reference:
//! `sinnet3000/teleport-client`, MIT — validated end-to-end on 2026-07-30). Stage ① implemented here is the
//! invite; later stages add broker pairing (`cloudaccess.svc.ui.com/teleport`), ICE/STUN nomination, and the
//! userspace-WireGuard data plane. Permissive crates only (ADR-0007): `boringtun`/`str0m`/`smoltcp`.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use crate::model::WireguardConfig;
use crate::wg::KeyPair;
use crate::{Error, Result};

pub mod dataplane;
pub mod ice;
pub mod nomination;
pub mod stun;

/// Host a Teleport invite URL must use, so a stray link can't send us pairing somewhere else.
const INVITE_HOST: &str = "teleport.ui.link";

/// Broker base for the Teleport signaling API.
pub const BROKER_BASE: &str = "https://cloudaccess.svc.ui.com/teleport";

/// Fixed scrypt salt the broker uses to turn an invite secret into a request token (from the reference
/// client — it's the literal ASCII bytes of this hex string, **not** the decoded hex).
const TOKEN_SALT: &[u8] = b"52D1FCE0AE4E5E5C8EF15BAE64A0FA570257BD6F48C7F9CD3FC82A26DB5E2976496A27971D7C23C6E6628E712C4E944BBD6DB79AACBA2369D31EB6438AD422FA";

/// A parsed Teleport invite: a console-generated capability, **single-use** for the first pairing (afterwards
/// a saved session is reused — see the planned session store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The invite UUID, canonicalised to lowercase.
    pub id: String,
}

impl Invite {
    /// Parse what a user pastes from the browser: a full `https://teleport.ui.link/<uuid>` URL (query/fragment
    /// tolerated), or a bare UUID. Any other host, or a non-UUID, is rejected as [`Error::InvalidInvite`].
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim();
        let raw = if s.contains("://") {
            let url = url::Url::parse(s).map_err(|_| invalid(input))?;
            if !url.host_str().is_some_and(|h| h.eq_ignore_ascii_case(INVITE_HOST)) {
                return Err(invalid(input));
            }
            url.path_segments()
                .and_then(|mut segs| segs.next())
                .unwrap_or_default()
                .to_owned()
        } else {
            // Bare UUID (tolerate a stray `?…`/`#…` if someone pasted a partial URL without the scheme).
            s.split(['?', '#']).next().unwrap_or(s).trim().to_owned()
        };
        let id = raw.to_ascii_lowercase();
        if is_uuid(&id) {
            Ok(Invite { id })
        } else {
            Err(invalid(input))
        }
    }

    /// The broker request token derived from this invite (its UUID is the "secret"). Sent as the
    /// `?token=` query param on every signaling call.
    pub fn token(&self) -> Result<String> {
        secret_to_token(&self.id)
    }
}

/// Derive a broker request token from an invite secret: `base64url(sha512(scrypt(secret, salt, N=2^14,
/// r=8, p=1, len=64)))`, no padding. Matches the reference client's `secretToToken`.
pub fn secret_to_token(secret: &str) -> Result<String> {
    let params = scrypt::Params::new(14, 8, 1, 64)
        .map_err(|e| Error::Other(anyhow::anyhow!("scrypt params: {e}")))?;
    let mut dk = [0u8; 64];
    scrypt::scrypt(secret.as_bytes(), TOKEN_SALT, &params, &mut dk)
        .map_err(|e| Error::Other(anyhow::anyhow!("scrypt: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha512::digest(dk)))
}

/// A response from the Teleport broker (subset we use). Unknown fields are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BrokerResponse {
    #[serde(rename = "teleportRequestId", default)]
    pub request_id: String,
    #[serde(rename = "response_type", default)]
    pub response_type: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub secret: String,
    #[serde(rename = "ice_configuration", default)]
    pub ice: Vec<IceServer>,
    #[serde(rename = "server_info", default)]
    pub server_info: ServerInfo,
    #[serde(rename = "client_ip", default)]
    pub client_ip: String,
    #[serde(rename = "dns_addrs", default)]
    pub dns_addrs: Vec<String>,
}

impl BrokerResponse {
    /// Bridge a `CONNECT_RESPONSE` into a [`WireguardConfig`]: the console's key + the tunnel address/DNS it
    /// assigned us, our device key, and the ICE-nominated `endpoint` (`host:port`). AllowedIPs defaults to
    /// full-tunnel and keepalive to 25s (what Teleport uses); the client tunnel address is a `/32` when the
    /// broker doesn't state a mask. This is the hand-off from signaling to the data plane.
    pub fn to_wireguard_config(&self, keypair: &KeyPair, endpoint: &str) -> WireguardConfig {
        let si = &self.server_info;
        let address = if si.tunnel_addr.is_empty() {
            Vec::new()
        } else {
            let mask = if si.tunnel_mask == 0 { 32 } else { si.tunnel_mask };
            vec![format!("{}/{}", si.tunnel_addr, mask)]
        };
        WireguardConfig {
            server_public_key: si.wg_pub_key.clone(),
            endpoint: endpoint.to_string(),
            allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            preshared_key: None,
            persistent_keepalive: Some(25),
            address,
            dns: self.dns_addrs.clone(),
            client_private_key: Some(keypair.private.clone()),
        }
    }
}

/// A STUN/TURN server from the broker's `ice_configuration`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IceServer {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

/// An ICE candidate: an address to try reaching the peer at (or that the peer can reach us at).
/// `kind` is `"iface"` (a local/host address), `"reflex"` (a STUN-observed public address), or `"turn"`
/// (a relay). `addr` is `host:port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(rename = "type")]
    pub kind: String,
    pub addr: String,
}

impl Candidate {
    /// Whether `addr` is an IPv6 socket address.
    pub fn is_ipv6(&self) -> bool {
        self.addr.parse::<SocketAddr>().map(|s| s.is_ipv6()).unwrap_or(false)
    }
}

/// A peer descriptor exchanged during pairing: a side's candidates, its optional ICE config, and whether
/// it is the nomination master (the console is master; we are the slave).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerDesc {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(rename = "ice_config", default, skip_serializing_if = "Vec::is_empty")]
    pub ice_config: Vec<IceServer>,
    #[serde(rename = "is_master", default)]
    pub is_master: bool,
}

/// The console side of the tunnel: its WireGuard public key, the tunnel address it assigns us, and its
/// candidate set (`peer_desc`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerInfo {
    #[serde(rename = "peer_desc", default)]
    pub peer_desc: PeerDesc,
    #[serde(rename = "wg_pub_key", default)]
    pub wg_pub_key: String,
    #[serde(rename = "udp_echo_port", default)]
    pub udp_echo_port: u16,
    #[serde(rename = "udp_echo_addr", default)]
    pub udp_echo_addr: String,
    #[serde(rename = "tunnel_mask", default)]
    pub tunnel_mask: u8,
    #[serde(rename = "tunnel_addr", default)]
    pub tunnel_addr: String,
}

/// Order candidates the way the reference client tries them: host (`iface`) before STUN-reflexive before
/// TURN before anything else, with a slight IPv6 preference. Ports `rankCandidates`. Stable, so ties keep
/// the broker's order.
pub fn rank_candidates(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut out = candidates.to_vec();
    out.sort_by_key(candidate_score);
    out
}

fn candidate_score(c: &Candidate) -> i32 {
    let base = match c.kind.as_str() {
        "iface" => 0,
        "reflex" => 10,
        "turn" => 20,
        _ => 100,
    };
    if c.is_ipv6() {
        base - 2
    } else {
        base
    }
}

/// A paired Teleport session, obtained by redeeming an invite (`REQUEST_ACCESS` → `ACCESS_GRANTED`). The
/// `token`/`secret` authenticate subsequent signaling; `device_token` is the invite-derived token. It's
/// reusable, so it's serialized and kept (keyring) — the single-use invite is only needed once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub secret: String,
    pub device_token: String,
}

/// Random standard-base64 of `n` bytes — the per-connection STUN session secret (`randomB64`).
fn random_b64(n: usize) -> String {
    use rand_core::RngCore;
    let mut b = vec![0u8; n];
    rand_core::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Poll interval for the console-facing signaling loops (`responsePollInterval`).
const POLL_INTERVAL: Duration = Duration::from_millis(600);

/// A random v4 UUID (for the `client_id` sent with `REQUEST_ACCESS`). Ports the reference `randomUUID`.
fn random_uuid() -> String {
    use rand_core::RngCore;
    let mut b = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |r: &[u8]| r.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!("{}-{}-{}-{}-{}", h(&b[0..4]), h(&b[4..6]), h(&b[6..8]), h(&b[8..10]), h(&b[10..16]))
}

/// Client for the Teleport signaling broker ([`BROKER_BASE`]). Ports the reference client's transport:
/// every call carries the invite-derived `?token=`, a `202 Accepted` / empty body means "still pending",
/// and [`Broker::poll`] waits for a target `response_type`. The `connect` offer itself (client WireGuard key
/// + gathered ICE candidates) is assembled by the ICE stage and passed as the request body.
pub struct Broker {
    http: reqwest::Client,
    base: String,
}

impl Default for Broker {
    fn default() -> Self {
        Self { http: reqwest::Client::new(), base: BROKER_BASE.to_string() }
    }
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the broker at a custom base (used by tests).
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), base: base.into() }
    }

    /// One signaling request to `{base}{path}?token={token}` with an optional JSON body. A `202 Accepted`
    /// or empty body yields a default (still-pending) [`BrokerResponse`], matching the reference client.
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<BrokerResponse> {
        let mut req = self.http.request(method, format!("{}{}", self.base, path));
        if !token.is_empty() {
            req = req.query(&[("token", token)]);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| Error::Http(e.to_string()))?;
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(Error::Http(format!("teleport broker returned HTTP {}", status.as_u16())));
        }
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(BrokerResponse::default());
        }
        let text = resp.text().await.map_err(|e| Error::Http(e.to_string()))?;
        if text.trim().is_empty() {
            return Ok(BrokerResponse::default());
        }
        serde_json::from_str(&text).map_err(|e| Error::Http(e.to_string()))
    }

    /// Poll `GET /{request_id}` every `interval` until a response's `response_type` equals `want`, up to
    /// `max_tries` attempts. Returns `Ok(None)` if it never arrives in time.
    pub async fn poll(
        &self,
        token: &str,
        request_id: &str,
        want: &str,
        interval: Duration,
        max_tries: u32,
    ) -> Result<Option<BrokerResponse>> {
        for _ in 0..max_tries {
            tokio::time::sleep(interval).await;
            let r = self.request(reqwest::Method::GET, &format!("/{request_id}"), token, None).await?;
            if r.response_type == want {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Redeem an invite into a paired [`Session`]: `POST /` with a `REQUEST_ACCESS` envelope (no query
    /// token — the envelope carries the invite-derived token), then poll `/{requestId}` for `ACCESS_GRANTED`.
    /// **Single-use: this consumes the invite's pairing capability.** Ports the reference `establishSession`.
    pub async fn pair(&self, invite: &Invite, client_name: &str) -> Result<Session> {
        let device_token = invite.token()?;
        let body = serde_json::json!({
            "token": device_token,
            "payload": {
                "request_type": "REQUEST_ACCESS",
                "secret": invite.id,
                "client_id": random_uuid(),
                "client_name": client_name,
            }
        });
        let access = self.request(reqwest::Method::POST, "/", "", Some(&body)).await?;
        if access.request_id.is_empty() {
            return Err(Error::Other(anyhow::anyhow!("teleport: REQUEST_ACCESS returned no request id")));
        }
        let granted = self
            .poll(&device_token, &access.request_id, "ACCESS_GRANTED", Duration::from_secs(2), 60)
            .await?
            .ok_or_else(|| Error::Other(anyhow::anyhow!("teleport: timed out waiting for ACCESS_GRANTED")))?;
        if granted.token.is_empty() {
            return Err(Error::Other(anyhow::anyhow!("teleport: access granted without a session token")));
        }
        Ok(Session { token: granted.token, secret: granted.secret, device_token })
    }

    /// Fetch the ICE (STUN/TURN) configuration for a paired session (`GET_ICE_CONFIGURATION`).
    pub async fn fetch_ice(&self, session: &Session) -> Result<Vec<IceServer>> {
        let body = serde_json::json!({
            "token": session.token,
            "payload": { "request_type": "GET_ICE_CONFIGURATION", "secret": session.secret }
        });
        let req = self.request(reqwest::Method::POST, "/", "", Some(&body)).await?;
        let resp = self
            .poll(&session.token, &req.request_id, "ICE_CONFIGURATION", POLL_INTERVAL, 100)
            .await?
            .ok_or_else(|| Error::Other(anyhow::anyhow!("teleport: timed out waiting for ICE_CONFIGURATION")))?;
        let _ = self
            .request(reqwest::Method::DELETE, &format!("/{}", req.request_id), &session.token, None)
            .await;
        Ok(resp.ice)
    }

    /// Send our connect offer (WG public key + a per-connection STUN session secret + our candidates + the
    /// ICE config) and await the console's `CONNECT_RESPONSE` (its candidates + WireGuard key + tunnel
    /// address). Ports `connectAndAwaitResponse`.
    pub async fn connect(
        &self,
        session: &Session,
        wg_pub_key: &str,
        stun_secret: &str,
        client_name: &str,
        local: &[Candidate],
        ice: &[IceServer],
    ) -> Result<BrokerResponse> {
        let peer_desc = PeerDesc { candidates: local.to_vec(), ice_config: ice.to_vec(), is_master: false };
        let body = serde_json::json!({
            "token": session.token,
            "payload": {
                "request_type": "CONNECT",
                "secret": session.secret,
                "client_name": client_name,
                "client_info": {
                    "wg_pub_key": wg_pub_key,
                    "stun_session_secret": stun_secret,
                    "peer_desc": peer_desc,
                }
            }
        });
        let req = self.request(reqwest::Method::POST, "/", "", Some(&body)).await?;
        let resp = self
            .poll(&session.token, &req.request_id, "CONNECT_RESPONSE", POLL_INTERVAL, 200)
            .await?
            .ok_or_else(|| Error::Other(anyhow::anyhow!("teleport: timed out waiting for CONNECT_RESPONSE")))?;
        let _ = self
            .request(reqwest::Method::DELETE, &format!("/{}", req.request_id), &session.token, None)
            .await;
        Ok(resp)
    }
}

/// A per-connection STUN session secret (exposed for callers that drive connect + nomination together).
pub fn new_stun_secret() -> String {
    random_b64(32)
}

/// How long to answer the console's nomination probes before giving up.
const NOMINATION_TIMEOUT: Duration = Duration::from_secs(45);

/// Pick a STUN server (`host:port`) from the ICE config, falling back to Cloudflare's public STUN.
fn stun_server(ice: &[IceServer]) -> String {
    ice.iter()
        .flat_map(|s| &s.urls)
        .find_map(|u| u.strip_prefix("stun:").map(|h| h.split('?').next().unwrap_or(h).to_string()))
        .unwrap_or_else(|| "stun.cloudflare.com:3478".to_string())
}

/// Run one full Teleport connection attempt from a paired [`Session`] to a **running** [`dataplane::Tunnel`]:
/// bind a fresh UDP socket, gather ICE candidates (host + reflexive), send the CONNECT offer, answer the
/// console's nomination, then bring up `if_name` as a TUN device carrying userspace WireGuard. This is the
/// single code path shared by the live probe and the daemon's Teleport backend; the reusable session comes
/// from a prior [`Broker::pair`] (invite redeem), so no invite is consumed here. Needs `CAP_NET_ADMIN` for the
/// TUN device.
pub async fn establish(broker: &Broker, session: &Session, if_name: &str) -> Result<dataplane::Tunnel> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("teleport: couldn't open a local socket: {e}")))?;
    let port = socket.local_addr().map_err(|e| Error::Other(anyhow::anyhow!("teleport: {e}")))?.port();

    let mut local = ice::local_candidates(port);
    let iceconf = broker.fetch_ice(session).await?;
    if let Some(reflex) = ice::reflexive_candidate(&socket, &stun_server(&iceconf)).await {
        tracing::info!(reflex = %reflex.addr, "teleport: reflexive candidate");
        local.push(reflex);
    }

    let kp = crate::wg::generate_keypair();
    let stun_secret = new_stun_secret();
    let resp = broker.connect(session, &kp.public, &stun_secret, if_name, &local, &iceconf).await?;
    if resp.server_info.wg_pub_key.is_empty() || resp.server_info.tunnel_addr.is_empty() {
        // The console accepted the request but returned no peer info — typically a still-active prior
        // connection on this session. Surface it as "couldn't connect" rather than hang on nomination.
        tracing::warn!("teleport: empty CONNECT_RESPONSE (a prior connection may still be active)");
        return Err(Error::VpnUnreachable);
    }
    tracing::info!(tunnel = %resp.server_info.tunnel_addr, "teleport: connected; awaiting nomination");

    let nominated = nomination::await_nomination(&socket, &stun_secret, NOMINATION_TIMEOUT)
        .await
        .ok_or(Error::VpnUnreachable)?;
    tracing::info!(%nominated, "teleport: endpoint nominated; bringing up tunnel");
    let wg_config = resp.to_wireguard_config(&kp, &nominated.to_string());
    dataplane::Tunnel::start(socket, nominated, &wg_config, if_name).await
}

fn invalid(input: &str) -> Error {
    Error::InvalidInvite(input.to_string())
}

/// True if `s` is a canonical 8-4-4-4-12 hex UUID (lowercase already applied by the caller).
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "08c9dc13-64bc-4525-9d9d-4659e6286f09";

    #[test]
    fn parses_a_full_invite_url() {
        assert_eq!(Invite::parse(&format!("https://teleport.ui.link/{UUID}")).unwrap().id, UUID);
    }

    #[test]
    fn parses_url_with_query_and_whitespace() {
        // Firebase appends `?l=1`; users paste with stray spaces/newlines.
        let pasted = format!("  https://teleport.ui.link/{UUID}?l=1\n");
        assert_eq!(Invite::parse(&pasted).unwrap().id, UUID);
    }

    #[test]
    fn parses_a_bare_uuid_and_lowercases() {
        assert_eq!(Invite::parse(UUID).unwrap().id, UUID);
        assert_eq!(Invite::parse(&UUID.to_uppercase()).unwrap().id, UUID);
    }

    #[test]
    fn rejects_wrong_host() {
        // A look-alike host must not be accepted — we'd pair against the wrong place.
        assert!(matches!(
            Invite::parse(&format!("https://evil.example.com/{UUID}")),
            Err(Error::InvalidInvite(_))
        ));
    }

    #[test]
    fn rejects_non_uuid() {
        for bad in ["", "not-an-invite", "https://teleport.ui.link/", "12345"] {
            assert!(matches!(Invite::parse(bad), Err(Error::InvalidInvite(_))), "should reject {bad:?}");
        }
    }

    #[test]
    fn secret_to_token_matches_known_vector() {
        // Cross-checked against Python's hashlib.scrypt — proves our derivation matches the server's.
        assert_eq!(
            secret_to_token("00000000-0000-4000-8000-000000000000").unwrap(),
            "uY-gn5-lU0Kkw-mEe4qIZU95wZNgLNoBVwrIKXuLdruGV1bCB18O07pnFzI-DPtNdLvBTdCRYB52fCEE6klGhQ",
        );
    }

    #[test]
    fn invite_token_uses_the_canonical_lowercase_uuid() {
        let via_url = Invite::parse(&format!("https://teleport.ui.link/{UUID}")).unwrap().token().unwrap();
        assert_eq!(via_url, secret_to_token(UUID).unwrap());
        // Uppercase input yields the same token (canonicalised before hashing).
        assert_eq!(Invite::parse(&UUID.to_uppercase()).unwrap().token().unwrap(), via_url);
    }

    #[test]
    fn deserializes_a_broker_connect_response() {
        let json = serde_json::json!({
            "teleportRequestId": "38b05983-cbeb-4a76-916c-4f74734f1db6",
            "response_type": "CONNECT_RESPONSE",
            "ice_configuration": [{"urls": ["turn:turn.cloudflare.com:3478"], "username": "u", "credential": "c"}],
            "server_info": {"wg_pub_key": "65CLAplnHZ+gXqFFqBigsVqCDcU4ZpRPu4IeuFrngD4=", "tunnel_addr": "192.168.2.7", "tunnel_mask": 32},
            "client_ip": "1.2.3.4",
            "dns_addrs": ["192.168.1.1"],
            "unknown_future_field": true
        });
        let r: BrokerResponse = serde_json::from_value(json).unwrap();
        assert_eq!(r.response_type, "CONNECT_RESPONSE");
        assert_eq!(r.request_id, "38b05983-cbeb-4a76-916c-4f74734f1db6");
        assert_eq!(r.server_info.wg_pub_key, "65CLAplnHZ+gXqFFqBigsVqCDcU4ZpRPu4IeuFrngD4=");
        assert_eq!(r.server_info.tunnel_mask, 32);
        assert_eq!(r.ice[0].urls, ["turn:turn.cloudflare.com:3478"]);
        assert_eq!(r.dns_addrs, ["192.168.1.1"]);
    }

    #[tokio::test]
    async fn broker_request_sends_the_token_and_parses_the_body() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/connect"))
            .and(query_param("token", "TOK"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "teleportRequestId": "req-1", "response_type": "SESSION_CREATED"
            })))
            .mount(&server)
            .await;

        let broker = Broker::with_base(server.uri());
        let body = serde_json::json!({"request_type": "CONNECT"});
        let r = broker
            .request(reqwest::Method::POST, "/connect", "TOK", Some(&body))
            .await
            .unwrap();
        assert_eq!(r.request_id, "req-1");
        assert_eq!(r.response_type, "SESSION_CREATED");
    }

    #[tokio::test]
    async fn broker_poll_returns_on_the_target_response_type() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/req-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response_type": "CONNECT_RESPONSE", "server_info": {"wg_pub_key": "K"}
            })))
            .mount(&server)
            .await;

        let broker = Broker::with_base(server.uri());
        let got = broker
            .poll("TOK", "req-1", "CONNECT_RESPONSE", Duration::from_millis(1), 3)
            .await
            .unwrap();
        assert_eq!(got.unwrap().server_info.wg_pub_key, "K");
    }

    #[test]
    fn builds_a_wireguard_config_from_a_connect_response() {
        let mut resp = BrokerResponse { response_type: "CONNECT_RESPONSE".into(), ..Default::default() };
        resp.server_info.wg_pub_key = "SRVKEY".into();
        resp.server_info.tunnel_addr = "192.168.2.7".into();
        resp.server_info.tunnel_mask = 32;
        resp.dns_addrs = vec!["192.168.1.1".into()];

        let kp = crate::wg::generate_keypair();
        let cfg = resp.to_wireguard_config(&kp, "192.168.60.1:54313");

        assert_eq!(cfg.server_public_key, "SRVKEY");
        assert_eq!(cfg.endpoint, "192.168.60.1:54313");
        assert_eq!(cfg.address, ["192.168.2.7/32"]);
        assert_eq!(cfg.dns, ["192.168.1.1"]);
        assert!(cfg.is_full_tunnel());

        // Renders the same shape the reference client produced end-to-end.
        let conf = cfg.to_wg_quick().unwrap();
        assert!(conf.contains("PublicKey = SRVKEY"));
        assert!(conf.contains("Endpoint = 192.168.60.1:54313"));
        assert!(conf.contains("Address = 192.168.2.7/32"));
        assert!(conf.contains("PersistentKeepalive = 25"));
        assert!(conf.contains(&format!("PrivateKey = {}", kp.private)));
    }

    fn cand(kind: &str, addr: &str) -> Candidate {
        Candidate { kind: kind.into(), addr: addr.into() }
    }

    #[test]
    fn ranks_candidates_host_then_reflex_then_turn() {
        let input = [
            cand("turn", "1.1.1.1:1"),
            cand("reflex", "2.2.2.2:2"),
            cand("iface", "192.168.1.1:3"),
            cand("weird", "3.3.3.3:4"),
        ];
        let ranked = rank_candidates(&input);
        let kinds: Vec<&str> = ranked.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["iface", "reflex", "turn", "weird"]);
    }

    #[test]
    fn ipv6_is_detected_and_slightly_preferred_within_a_type() {
        assert!(cand("iface", "[fd00::1]:5").is_ipv6());
        assert!(!cand("iface", "192.168.1.1:5").is_ipv6());
        let ranked = rank_candidates(&[cand("iface", "192.168.1.1:5"), cand("iface", "[fd00::1]:5")]);
        assert_eq!(ranked[0].addr, "[fd00::1]:5");
    }

    #[test]
    fn parses_the_server_peer_desc_candidates() {
        let json = serde_json::json!({
            "response_type": "CONNECT_RESPONSE",
            "server_info": { "wg_pub_key": "K", "peer_desc": { "is_master": true, "candidates": [
                {"type": "iface", "addr": "192.168.60.1:54313"},
                {"type": "reflex", "addr": "128.76.158.114:54313"}
            ]}}
        });
        let r: BrokerResponse = serde_json::from_value(json).unwrap();
        assert!(r.server_info.peer_desc.is_master);
        assert_eq!(r.server_info.peer_desc.candidates.len(), 2);
        assert_eq!(r.server_info.peer_desc.candidates[0].addr, "192.168.60.1:54313");
    }

    #[tokio::test]
    async fn pair_redeems_an_invite_into_a_session() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "teleportRequestId": "acc-1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/acc-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response_type": "ACCESS_GRANTED", "token": "sess-tok", "secret": "sess-sec"
            })))
            .mount(&server)
            .await;

        // Speed the poll's first sleep up via a paused clock (tokio auto-advances when nothing else is ready).
        tokio::time::pause();
        let broker = Broker::with_base(server.uri());
        let invite = Invite::parse(UUID).unwrap();
        let session = broker.pair(&invite, "test-client").await.unwrap();
        assert_eq!(session.token, "sess-tok");
        assert_eq!(session.secret, "sess-sec");
        assert_eq!(session.device_token, invite.token().unwrap());
    }
}
