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

use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha512};

use crate::{Error, Result};

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

/// A STUN/TURN server from the broker's `ice_configuration`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IceServer {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

/// The console side of the tunnel: its WireGuard public key + the tunnel address it assigns us.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerInfo {
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
}
