//! Orchestration engine: ties SSO/UCS + the system backends into the user-visible state machine
//! ([`Snapshot`]). This is the heart of `ternd`, but it lives in `tern-core` and is platform-agnostic, so it
//! can be driven end-to-end on any platform with a mock UCS server + the in-memory [`StubBackend`].
//!
//! Flow (docs/02 UPDATE): browser SSO yields a bearer token → [`Engine::sign_in`] fetches identity + hosts →
//! [`Engine::connect`] ensures a device keypair, enrolls the public key, provisions a `vpn/session` (a
//! WireGuard config), brings the tunnel up via the VPN backend, then auto-mounts the selected+reachable drives.

use std::sync::Arc;

use crate::backend::{MountBackend, Reach, Reachability, SecretStore, VpnBackend};
use crate::config::Config;
use crate::model::{Drive, Host};
use crate::state::{Access, Auth, DriveMount, DriveStatus, Snapshot};
use crate::ucs::UcsClient;
use crate::{wg, Error, Result};

/// Keyring keys (values live in the OS keyring via [`SecretStore`], never in config/logs).
const TOKEN_KEY: &str = "sso_token";
const WG_PRIVATE_KEY: &str = "wg_private_key";

/// Owns session + orchestration state and drives the backends. Not `Clone`; the daemon wraps it in a lock.
pub struct Engine {
    ucs: UcsClient,
    vpn: Arc<dyn VpnBackend>,
    mounts: Arc<dyn MountBackend>,
    reach: Arc<dyn Reachability>,
    secrets: Arc<dyn SecretStore>,
    config: Config,
    auth: Auth,
    access: Access,
    hosts: Vec<Host>,
    drives: Vec<Drive>,
    active_console: Option<String>,
}

impl Engine {
    pub fn new(
        ucs: UcsClient,
        vpn: Arc<dyn VpnBackend>,
        mounts: Arc<dyn MountBackend>,
        reach: Arc<dyn Reachability>,
        secrets: Arc<dyn SecretStore>,
        config: Config,
    ) -> Self {
        Self {
            ucs,
            vpn,
            mounts,
            reach,
            secrets,
            config,
            auth: Auth::SignedOut,
            access: Access::Off,
            hosts: Vec::new(),
            drives: Vec::new(),
            active_console: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// Complete sign-in given a bearer token obtained from browser SSO (ADR-0009).
    pub async fn sign_in(&mut self, token: String) -> Result<()> {
        self.auth = Auth::SigningIn;
        self.secrets.set(TOKEN_KEY, &token).await?;
        self.ucs.set_token(Some(token));
        let identity = self.ucs.identity().await?;
        self.hosts = self.ucs.hosts().await?;
        self.auth = Auth::SignedIn(identity);
        Ok(())
    }

    /// Restore a saved session token at startup (for "connect at startup"), if one is stored.
    pub async fn restore_session(&mut self) -> Result<bool> {
        match self.secrets.get(TOKEN_KEY).await? {
            Some(token) => {
                self.sign_in(token).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn sign_out(&mut self) -> Result<()> {
        let _ = self.disconnect().await;
        self.secrets.delete(TOKEN_KEY).await?;
        self.ucs.set_token(None);
        self.auth = Auth::SignedOut;
        self.hosts.clear();
        self.drives.clear();
        Ok(())
    }

    /// Turn on Access: enroll our key, provision a VPN session, bring up the tunnel, and auto-mount drives.
    pub async fn connect(&mut self, console_id: &str) -> Result<()> {
        // Empty id → default to the first available console (single-site convenience for the switch/tray).
        let console_id = if console_id.is_empty() {
            self.hosts
                .first()
                .map(|h| h.console_id.clone())
                .ok_or(Error::NoConsoleAvailable)?
        } else {
            console_id.to_string()
        };
        self.access = Access::TurningOn;
        let public_key = self.ensure_keypair().await?;
        self.ucs.enroll_public_key(&public_key).await?;
        let mut wg = self.ucs.create_vpn_session(&console_id, &public_key).await?.wg;
        if !wg.has_dialable_endpoint() {
            // Console is relay-only (needs UniFi's proprietary bridge) — out of scope; be honest, don't fake.
            self.access = Access::Unreachable;
            return Err(Error::RelayOnly);
        }
        // Inject our device private key (from the keyring) so the backend can build the tunnel.
        wg.client_private_key = self.secrets.get(WG_PRIVATE_KEY).await?;
        self.vpn.connect(&wg).await?;
        if !self.vpn.is_active().await? {
            self.access = Access::Unreachable;
            return Err(Error::VpnUnreachable);
        }
        self.access = Access::On;
        self.active_console = Some(console_id.to_string());
        // Best-effort drive discovery (endpoint unconfirmed) + auto-mount of the selected, reachable ones.
        self.drives = self.ucs.drives(&console_id).await.unwrap_or_default();
        self.mount_selected().await;
        Ok(())
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        for d in &self.drives {
            let _ = self.mounts.unmount(d).await;
        }
        self.vpn.disconnect().await?;
        self.access = Access::Off;
        self.active_console = None;
        Ok(())
    }

    /// Mount every selected + reachable drive (idempotent; safe to re-run on network changes).
    pub async fn mount_selected(&self) {
        let host = self.active_host();
        for d in &self.drives {
            if d.encrypted || !self.config.auto_mount_drives.iter().any(|id| id == &d.id) {
                continue;
            }
            let reachable = match host {
                Some(h) => self.reach.reach(h).await != Reach::Unreachable,
                None => false,
            };
            if reachable {
                let _ = self.mounts.mount(d).await;
            }
        }
    }

    /// Return our device public key, generating + persisting a keypair on first use.
    async fn ensure_keypair(&self) -> Result<String> {
        if let Some(priv_b64) = self.secrets.get(WG_PRIVATE_KEY).await? {
            if let Some(public) = wg::public_from_private(&priv_b64) {
                return Ok(public);
            }
        }
        let kp = wg::generate_keypair();
        self.secrets.set(WG_PRIVATE_KEY, &kp.private).await?;
        Ok(kp.public)
    }

    fn active_host(&self) -> Option<&Host> {
        let id = self.active_console.as_deref()?;
        self.hosts.iter().find(|h| h.console_id == id)
    }

    /// A consistent snapshot for the UI (tray/GUI/CLI render only this).
    pub async fn snapshot(&self) -> Snapshot {
        let mounted = self.mounts.mounted().await.unwrap_or_default();
        let drives = self
            .drives
            .iter()
            .map(|d| {
                let selected = self.config.auto_mount_drives.iter().any(|id| id == &d.id);
                let state = if mounted.contains(&d.id) {
                    DriveMount::Mounted
                } else if d.encrypted {
                    DriveMount::Locked
                } else if selected {
                    if self.access == Access::On {
                        DriveMount::Reachable
                    } else {
                        DriveMount::Unavailable
                    }
                } else {
                    DriveMount::Idle
                };
                DriveStatus { drive: d.clone(), state }
            })
            .collect();
        Snapshot { auth: self.auth.clone(), access: self.access, drives }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::StubBackend;
    use crate::ucs::Endpoints;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn engine_for(server: &MockServer, config: Config) -> Engine {
        let ucs = UcsClient::new(Endpoints { sso: "https://sso.ui.com".into(), api_gw: server.uri() });
        let stub = Arc::new(StubBackend::new());
        Engine::new(ucs, stub.clone(), stub.clone(), stub.clone(), stub, config)
    }

    async fn happy_server() -> MockServer {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxy/users/public/api/v2/identity/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "email": "jah@example.com", "displayName": "Jakob", "organization": "Acme"
            })))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/user-token/hosts/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"consoleStandardId": "c1", "hostName": "Home"}
            ])))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/proxy/users/public/api/v2/identity/public_key"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/proxy/ucs/public/user/api/v1/vpn/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sessionId": "sess-1",
                "wgConfig": {
                    "serverPublicKey": "srv", "endpoint": "203.0.113.7:51820",
                    "allowedIps": ["10.0.0.0/8"], "persistentKeepalive": 25, "clientAddress": ["10.2.0.9/32"]
                }
            })))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/proxy/ucs/public/user/api/v1/drive/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "d1", "name": "Design"},
                {"id": "d2", "name": "Archive"}
            ])))
            .mount(&s)
            .await;
        s
    }

    #[tokio::test]
    async fn full_flow_sign_in_connect_and_selective_automount() {
        let server = happy_server().await;
        let mut config = Config::default();
        config.set_auto_mount("d1", true); // only d1 auto-mounts; d2 is available but not selected
        let mut engine = engine_for(&server, config);

        engine.sign_in("jwt-token".into()).await.unwrap();
        assert!(matches!(engine.snapshot().await.auth, Auth::SignedIn(_)));

        engine.connect("").await.unwrap(); // empty → defaults to the first host (c1)
        let snap = engine.snapshot().await;
        assert_eq!(snap.access, Access::On);
        assert_eq!(snap.summary_line(), "Access on · 1 drive mounted");

        let d1 = snap.drives.iter().find(|d| d.drive.id == "d1").unwrap();
        let d2 = snap.drives.iter().find(|d| d.drive.id == "d2").unwrap();
        assert_eq!(d1.state, DriveMount::Mounted, "selected + reachable drive should mount");
        assert_eq!(d2.state, DriveMount::Idle, "unselected drive stays idle");

        engine.disconnect().await.unwrap();
        assert_eq!(engine.snapshot().await.access, Access::Off);
    }

    #[tokio::test]
    async fn relay_only_console_is_reported_not_faked() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/proxy/users/public/api/v2/identity/public_key"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/proxy/ucs/public/user/api/v1/vpn/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                // no dialable endpoint => relay-only console (out of scope)
                "wgConfig": {"serverPublicKey": "srv", "endpoint": "", "allowedIps": []}
            })))
            .mount(&server)
            .await;

        let mut engine = engine_for(&server, Config::default());
        let err = engine.connect("c1").await.unwrap_err();
        assert!(matches!(err, Error::RelayOnly));
        // Honest state, not a fake "connected".
        assert_eq!(engine.snapshot().await.access, Access::Unreachable);
    }
}
