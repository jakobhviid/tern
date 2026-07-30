//! Domain types mirroring the UniFi Identity ("UniFi Endpoint") API surface.
//!
//! Field names + shapes are derived from static analysis of the macOS client binary (see
//! `docs/02-vpn-protocol-and-reference-clients.md`). The UCS API mixes `camelCase` and `snake_case` across
//! endpoints, so we accept both via serde `alias`. Shapes are HIGH-confidence on names, MEDIUM on exact
//! nesting — a single traffic capture on Bazzite will confirm and let us tighten these. Marked with
//! `#[serde(default)]` generously so an unexpected extra/missing field never breaks a whole response.

use serde::{Deserialize, Serialize};

/// The signed-in person (from the SSO/identity info endpoint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    #[serde(alias = "username", alias = "userEmail")]
    pub email: String,
    #[serde(default, alias = "displayName", alias = "fullName")]
    pub display_name: Option<String>,
    #[serde(default, alias = "org", alias = "organizationName")]
    pub organization: Option<String>,
}

/// A UniFi console/site the user can reach. Addressed by id, not IP (see docs/02); the WAN endpoint is only
/// known once the console reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    #[serde(alias = "consoleStandardId", alias = "consoleId", alias = "id")]
    pub console_id: String,
    #[serde(default, alias = "hostName", alias = "consoleName")]
    pub name: String,
    #[serde(default, alias = "wanIp")]
    pub wan_ip: Option<String>,
}

/// A WireGuard peer config as returned by `POST .../vpn/session` (field names per binary recon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireguardConfig {
    #[serde(alias = "serverPublicKey", alias = "server_public_key", alias = "publicKey")]
    pub server_public_key: String,
    /// `host:port` of the gateway's WireGuard endpoint.
    pub endpoint: String,
    #[serde(default, alias = "allowedIps", alias = "allowed_ips")]
    pub allowed_ips: Vec<String>,
    #[serde(default, alias = "presharedKey", alias = "preshared_key")]
    pub preshared_key: Option<String>,
    #[serde(default, alias = "persistentKeepalive", alias = "persistent_keepalive")]
    pub persistent_keepalive: Option<u16>,
    /// Address(es) assigned to us on the tunnel (client interface address, CIDR).
    #[serde(default, alias = "clientAddress", alias = "interfaceAddress", alias = "address")]
    pub address: Vec<String>,
    #[serde(default, alias = "dnsServers", alias = "dns")]
    pub dns: Vec<String>,
    /// Our device private key (base64), injected by the engine from the keyring before connecting. It never
    /// comes from the server, so it is skipped in (de)serialization.
    #[serde(skip)]
    pub client_private_key: Option<String>,
}

impl WireguardConfig {
    /// True if the gateway advertises a directly-dialable public endpoint (host:port). When false, the
    /// console is likely relay-only (out of scope — see docs/01/02).
    pub fn has_dialable_endpoint(&self) -> bool {
        // A dialable endpoint contains a ':' and a non-empty host part.
        self.endpoint
            .rsplit_once(':')
            .map(|(host, port)| !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty())
            .unwrap_or(false)
    }

    /// Whether this is a full-tunnel config (default route present) vs. split-tunnel.
    pub fn is_full_tunnel(&self) -> bool {
        self.allowed_ips.iter().any(|a| a == "0.0.0.0/0" || a == "::/0")
    }

    /// Render to a `wg-quick`-style `.conf` (used by the NetworkManager import path on Linux). Returns
    /// `None` if the device private key hasn't been injected yet.
    pub fn to_wg_quick(&self) -> Option<String> {
        let private = self.client_private_key.as_deref()?;
        let mut s = String::from("[Interface]\n");
        s.push_str(&format!("PrivateKey = {private}\n"));
        if !self.address.is_empty() {
            s.push_str(&format!("Address = {}\n", self.address.join(", ")));
        }
        if !self.dns.is_empty() {
            s.push_str(&format!("DNS = {}\n", self.dns.join(", ")));
        }
        s.push_str("\n[Peer]\n");
        s.push_str(&format!("PublicKey = {}\n", self.server_public_key));
        if let Some(psk) = &self.preshared_key {
            s.push_str(&format!("PresharedKey = {psk}\n"));
        }
        s.push_str(&format!("Endpoint = {}\n", self.endpoint));
        let allowed = if self.allowed_ips.is_empty() {
            "0.0.0.0/0, ::/0".to_string()
        } else {
            self.allowed_ips.join(", ")
        };
        s.push_str(&format!("AllowedIPs = {allowed}\n"));
        if let Some(k) = self.persistent_keepalive {
            s.push_str(&format!("PersistentKeepalive = {k}\n"));
        }
        Some(s)
    }
}

/// The response of provisioning a VPN session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnSession {
    #[serde(default, alias = "sessionId", alias = "id")]
    pub session_id: Option<String>,
    #[serde(alias = "wgConfig", alias = "wireguardConfig", alias = "config")]
    pub wg: WireguardConfig,
}

/// A UniFi Drive share the user may mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
    #[serde(alias = "driveId")]
    pub id: String,
    #[serde(alias = "driveName", alias = "displayName")]
    pub name: String,
    /// SMB share as `//host/share` or `smb://…`, once known (may require the tunnel to resolve the host).
    #[serde(default, alias = "smbPath", alias = "sharePath")]
    pub share: Option<String>,
    #[serde(default, alias = "isEncrypted")]
    pub encrypted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_vpn_session_camelcase() {
        // Shape approximating the UCS `vpn/session` response (camelCase variant).
        let json = serde_json::json!({
            "sessionId": "sess-abc123",
            "wgConfig": {
                "serverPublicKey": "c2VydmVycHVia2V5MDAwMDAwMDAwMDAwMDAwMDAwMD0=",
                "endpoint": "gw.example.ui.com:51820",
                "allowedIps": ["10.0.0.0/8", "192.168.1.0/24"],
                "presharedKey": "cHNrMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=",
                "persistentKeepalive": 25,
                "clientAddress": ["10.2.0.5/32"],
                "dnsServers": ["10.0.0.1"]
            }
        });
        let s: VpnSession = serde_json::from_value(json).unwrap();
        assert_eq!(s.session_id.as_deref(), Some("sess-abc123"));
        assert_eq!(s.wg.endpoint, "gw.example.ui.com:51820");
        assert_eq!(s.wg.persistent_keepalive, Some(25));
        assert!(s.wg.has_dialable_endpoint());
        assert!(!s.wg.is_full_tunnel(), "explicit subnets = split tunnel");
        assert_eq!(s.wg.address, vec!["10.2.0.5/32"]);
    }

    #[test]
    fn deserializes_vpn_session_snakecase_and_detects_full_tunnel() {
        let json = serde_json::json!({
            "id": "sess-2",
            "config": {
                "server_public_key": "k",
                "endpoint": "1.2.3.4:51820",
                "allowed_ips": ["0.0.0.0/0", "::/0"],
                "persistent_keepalive": 15,
                "address": ["10.9.9.9/32"]
            }
        });
        let s: VpnSession = serde_json::from_value(json).unwrap();
        assert_eq!(s.session_id.as_deref(), Some("sess-2"));
        assert!(s.wg.is_full_tunnel());
        assert!(s.wg.preshared_key.is_none());
    }

    #[test]
    fn host_accepts_multiple_id_spellings() {
        let a: Host = serde_json::from_value(serde_json::json!({"consoleStandardId":"c1"})).unwrap();
        let b: Host = serde_json::from_value(serde_json::json!({"id":"c1"})).unwrap();
        assert_eq!(a.console_id, "c1");
        assert_eq!(a.console_id, b.console_id);
    }

    #[test]
    fn renders_wg_quick_only_with_private_key() {
        let mut cfg: WireguardConfig = serde_json::from_value(serde_json::json!({
            "serverPublicKey": "srv",
            "endpoint": "1.2.3.4:51820",
            "allowedIps": ["10.0.0.0/8"],
            "persistentKeepalive": 25,
            "clientAddress": ["10.2.0.5/32"],
            "dnsServers": ["10.0.0.1"]
        }))
        .unwrap();
        assert!(cfg.to_wg_quick().is_none(), "no private key yet");

        cfg.client_private_key = Some("PRIVKEYB64".into());
        let conf = cfg.to_wg_quick().unwrap();
        assert!(conf.contains("[Interface]"));
        assert!(conf.contains("PrivateKey = PRIVKEYB64"));
        assert!(conf.contains("Address = 10.2.0.5/32"));
        assert!(conf.contains("DNS = 10.0.0.1"));
        assert!(conf.contains("PublicKey = srv"));
        assert!(conf.contains("Endpoint = 1.2.3.4:51820"));
        assert!(conf.contains("AllowedIPs = 10.0.0.0/8"));
        assert!(conf.contains("PersistentKeepalive = 25"));
    }
}
