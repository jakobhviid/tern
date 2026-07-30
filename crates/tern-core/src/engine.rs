//! Orchestration engine: ties SSO/UCS + the system backends into the user-visible state machine
//! ([`Snapshot`]). This is the heart of `ternd`, but it lives in `tern-core` and is platform-agnostic, so it
//! can be driven end-to-end on any platform with a mock UCS server + the in-memory [`StubBackend`].
//!
//! Flow (docs/02 UPDATE): browser SSO yields a bearer token → [`Engine::sign_in`] fetches identity + hosts →
//! [`Engine::connect`] ensures a device keypair, enrolls the public key, provisions a `vpn/session` (a
//! WireGuard config), brings the tunnel up via the VPN backend, then auto-mounts the selected+reachable drives.

use std::sync::Arc;

use crate::backend::{MountBackend, Reach, Reachability, SecretStore, TeleportVpn, VpnBackend};
use crate::config::Config;
use crate::model::{Drive, Host};
use crate::state::{Access, Auth, DriveMount, DriveStatus, Snapshot};
use crate::teleport::Session;
use crate::ucs::UcsClient;
use crate::{wg, Error, Result};

/// Keyring keys (values live in the OS keyring via [`SecretStore`], never in config/logs).
const TOKEN_KEY: &str = "sso_token";
const WG_PRIVATE_KEY: &str = "wg_private_key";
/// The reusable Teleport session (JSON), persisted so a redeemed invite reconnects without re-pairing.
const TELEPORT_SESSION_KEY: &str = "teleport_session";

/// Owns session + orchestration state and drives the backends. Not `Clone`; the daemon wraps it in a lock.
pub struct Engine {
    ucs: UcsClient,
    vpn: Arc<dyn VpnBackend>,
    teleport: Arc<dyn TeleportVpn>,
    mounts: Arc<dyn MountBackend>,
    reach: Arc<dyn Reachability>,
    secrets: Arc<dyn SecretStore>,
    config: Config,
    auth: Auth,
    access: Access,
    hosts: Vec<Host>,
    drives: Vec<Drive>,
    active_console: Option<String>,
    /// The last imported plain-WireGuard `.conf` (ADR-0004 fallback), remembered so the Access toggle can
    /// re-up it after a disconnect without an account/sign-in.
    imported_conf: Option<String>,
    /// The reusable Teleport session (ADR-0016), once an invite has been redeemed — the Access toggle brings
    /// its tunnel up without needing the invite again. Persisted in the keyring across restarts.
    teleport_session: Option<Session>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ucs: UcsClient,
        vpn: Arc<dyn VpnBackend>,
        teleport: Arc<dyn TeleportVpn>,
        mounts: Arc<dyn MountBackend>,
        reach: Arc<dyn Reachability>,
        secrets: Arc<dyn SecretStore>,
        config: Config,
    ) -> Self {
        Self {
            ucs,
            vpn,
            teleport,
            mounts,
            reach,
            secrets,
            config,
            auth: Auth::SignedOut,
            access: Access::Off,
            hosts: Vec::new(),
            drives: Vec::new(),
            active_console: None,
            imported_conf: None,
            teleport_session: None,
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
        let result = self.sign_in_inner(token).await;
        if result.is_err() {
            self.auth = Auth::SignedOut;
        }
        result
    }

    async fn sign_in_inner(&mut self, token: String) -> Result<()> {
        self.secrets.set(TOKEN_KEY, &token).await?;
        self.ucs.set_token(Some(token));
        let identity = self.ucs.identity().await?;
        self.hosts = self.ucs.hosts().await?;
        self.auth = Auth::SignedIn(identity);
        Ok(())
    }

    /// Mark that a browser sign-in is starting (shown as "Signing you in…").
    pub fn begin_sign_in(&mut self) {
        self.auth = Auth::SigningIn;
    }

    /// Roll back to signed-out if a sign-in attempt failed or was cancelled.
    pub fn cancel_sign_in(&mut self) {
        if matches!(self.auth, Auth::SigningIn) {
            self.auth = Auth::SignedOut;
        }
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
        // "Forget this console" is a full reset: also drop a redeemed Teleport session and any imported
        // config, so nothing reconnects afterwards without fresh credentials.
        let _ = self.secrets.delete(TELEPORT_SESSION_KEY).await;
        self.teleport_session = None;
        self.imported_conf = None;
        self.auth = Auth::SignedOut;
        self.hosts.clear();
        self.drives.clear();
        Ok(())
    }

    /// Turn on Access: enroll our key, provision a VPN session, bring up the tunnel, and auto-mount drives.
    pub async fn connect(&mut self, console_id: &str) -> Result<()> {
        // Teleport mode (ADR-0016): a redeemed session reconnects here without needing the invite again.
        if let Some(session) = self.teleport_session.clone() {
            self.access = Access::TurningOn;
            self.teleport.up(&session).await?;
            if !self.teleport.is_up().await? {
                self.access = Access::Unreachable;
                return Err(Error::VpnUnreachable);
            }
            self.access = Access::On;
            return Ok(());
        }
        // Imported-config mode (ADR-0004 fallback, no account): if the user imported a config this session,
        // the Access toggle reconnects it — resume the still-present profile if possible, else re-import.
        // (Gated on having an imported config so the account/UCS connect flow below is untouched.)
        if !matches!(self.auth, Auth::SignedIn(_)) {
            if let Some(conf) = self.imported_conf.clone() {
                self.access = Access::TurningOn;
                if self.vpn.resume().await.is_ok() && self.vpn.is_active().await.unwrap_or(false) {
                    self.access = Access::On;
                    return Ok(());
                }
                return self.import_wireguard(conf).await;
            }
        }
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
        // Tear down whichever tunnel is up: the Teleport data plane if a session drove it, else the VPN
        // backend (account or imported `.conf`).
        if self.teleport_session.is_some() {
            self.teleport.down().await?;
        } else {
            self.vpn.disconnect().await?;
        }
        self.access = Access::Off;
        self.active_console = None;
        Ok(())
    }

    /// Redeem a Teleport invite (ADR-0016): pair into a reusable session, persist it to the keyring, and
    /// bring the tunnel up. The invite is single-use; afterwards the stored session reconnects via the Access
    /// toggle without it. Independent of account sign-in.
    pub async fn redeem_invite(&mut self, url: &str) -> Result<()> {
        let invite = crate::teleport::Invite::parse(url)?;
        self.access = Access::TurningOn;
        let result = self.redeem_inner(&invite).await;
        if result.is_err() {
            self.access = Access::Off;
        }
        result
    }

    async fn redeem_inner(&mut self, invite: &crate::teleport::Invite) -> Result<()> {
        let session = self.teleport.redeem(invite).await?;
        let json = serde_json::to_string(&session)
            .map_err(|e| Error::Other(anyhow::anyhow!("couldn't serialize the session: {e}")))?;
        self.secrets.set(TELEPORT_SESSION_KEY, &json).await?;
        self.teleport.up(&session).await?;
        if !self.teleport.is_up().await? {
            self.access = Access::Unreachable;
            return Err(Error::VpnUnreachable);
        }
        self.teleport_session = Some(session);
        self.access = Access::On;
        Ok(())
    }

    /// Load a persisted Teleport session at startup, so a previously redeemed console can reconnect (via
    /// [`Engine::connect`]) without the single-use invite. Returns whether one was found.
    pub async fn restore_teleport_session(&mut self) -> Result<bool> {
        if let Some(json) = self.secrets.get(TELEPORT_SESSION_KEY).await? {
            if let Ok(session) = serde_json::from_str::<Session>(&json) {
                self.teleport_session = Some(session);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Forget the redeemed Teleport console: tear the tunnel down and drop the persisted session (a new
    /// invite is then required to reconnect).
    pub async fn forget_teleport(&mut self) -> Result<()> {
        let _ = self.teleport.down().await;
        self.secrets.delete(TELEPORT_SESSION_KEY).await?;
        self.teleport_session = None;
        self.access = Access::Off;
        Ok(())
    }

    /// Bring up a plain WireGuard `.conf` the user imported — the console's built-in WireGuard Server or any
    /// WireGuard peer (ADR-0004 fallback). Independent of the Teleport/account flow: parse, hand to the VPN
    /// backend, reflect the tunnel state. Does not change auth (no sign-in needed for a static config).
    pub async fn import_wireguard(&mut self, conf: String) -> Result<()> {
        let wg = crate::model::WireguardConfig::from_wg_quick(&conf)?;
        self.access = Access::TurningOn;
        self.vpn.connect(&wg).await?;
        if !self.vpn.is_active().await? {
            self.access = Access::Unreachable;
            return Err(Error::VpnUnreachable);
        }
        self.access = Access::On;
        self.imported_conf = Some(conf);
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
                DriveStatus { drive: d.clone(), state, selected }
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
        Engine::new(ucs, stub.clone(), stub.clone(), stub.clone(), stub.clone(), stub, config)
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

    #[tokio::test]
    async fn sign_in_failure_resets_to_signed_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxy/users/public/api/v2/identity/info"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let mut engine = engine_for(&server, Config::default());

        let err = engine.sign_in("bad-token".into()).await.unwrap_err();
        assert!(matches!(err, Error::SessionExpired));
        // Must not get stuck in "Signing you in…".
        assert!(matches!(engine.snapshot().await.auth, Auth::SignedOut));
    }

    #[tokio::test]
    async fn restore_session_is_false_without_a_saved_token() {
        let server = MockServer::start().await;
        let mut engine = engine_for(&server, Config::default());
        assert!(!engine.restore_session().await.unwrap());
    }

    const INVITE_UUID: &str = "08c9dc13-64bc-4525-9d9d-4659e6286f09";

    #[tokio::test]
    async fn redeeming_an_invite_pairs_persists_and_connects() {
        let server = MockServer::start().await;
        let mut engine = engine_for(&server, Config::default());

        engine.redeem_invite(INVITE_UUID).await.unwrap();
        assert_eq!(engine.snapshot().await.access, Access::On);
        // The session is persisted (so a restart can reconnect without the invite).
        assert!(engine.secrets.get(TELEPORT_SESSION_KEY).await.unwrap().is_some());

        // Access toggle: disconnect then reconnect uses the stored session, no invite needed.
        engine.disconnect().await.unwrap();
        assert_eq!(engine.snapshot().await.access, Access::Off);
        engine.connect("").await.unwrap();
        assert_eq!(engine.snapshot().await.access, Access::On);
    }

    #[tokio::test]
    async fn an_invalid_invite_is_rejected_and_resets_access() {
        let server = MockServer::start().await;
        let mut engine = engine_for(&server, Config::default());
        let err = engine.redeem_invite("not-an-invite").await.unwrap_err();
        assert!(matches!(err, Error::InvalidInvite(_)));
        assert_eq!(engine.snapshot().await.access, Access::Off);
    }

    #[tokio::test]
    async fn restoring_then_forgetting_a_teleport_session() {
        let server = MockServer::start().await;
        let mut engine = engine_for(&server, Config::default());
        engine.redeem_invite(INVITE_UUID).await.unwrap();

        // A fresh engine sharing the same keyring restores the session and can reconnect.
        let mut restored = engine_for(&server, Config::default());
        // Point it at the same secret store by reusing the persisted value.
        let json = engine.secrets.get(TELEPORT_SESSION_KEY).await.unwrap().unwrap();
        restored.secrets.set(TELEPORT_SESSION_KEY, &json).await.unwrap();
        assert!(restored.restore_teleport_session().await.unwrap());
        restored.connect("").await.unwrap();
        assert_eq!(restored.snapshot().await.access, Access::On);

        // Forget drops the persisted session; reconnect then needs a new invite.
        engine.forget_teleport().await.unwrap();
        assert!(engine.secrets.get(TELEPORT_SESSION_KEY).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn signing_out_also_forgets_a_teleport_console() {
        let server = MockServer::start().await;
        let mut engine = engine_for(&server, Config::default());
        engine.redeem_invite(INVITE_UUID).await.unwrap();

        // "Forget this console" (sign-out) is a full reset even for a Teleport-only user.
        engine.sign_out().await.unwrap();
        assert_eq!(engine.snapshot().await.access, Access::Off);
        assert!(engine.secrets.get(TELEPORT_SESSION_KEY).await.unwrap().is_none());
        // Nothing reconnects without a fresh invite.
        assert!(!engine.restore_teleport_session().await.unwrap());
    }
}
