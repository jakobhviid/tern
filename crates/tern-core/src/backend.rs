//! System-integration seams. The core orchestrates against these traits; `tern-linux` implements them with
//! NetworkManager (VPN), GVfs/`mount.cifs` (drives), and libsecret (secrets). The [`StubBackend`] is an
//! in-memory implementation so the whole flow can be exercised on any platform (macOS/CI) with no display,
//! D-Bus, or real UniFi account.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::model::{Drive, Host, WireguardConfig};
use crate::Result;

/// Where a target can be reached from right now (drives the reachability-gated mount logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Directly on the local network.
    Lan,
    /// Reachable because Access (the VPN) is up.
    Vpn,
    /// Not reachable at all right now.
    Unreachable,
}

/// Brings the WireGuard tunnel up/down. Linux impl stores a user-owned NetworkManager connection (ADR-0004).
#[async_trait]
pub trait VpnBackend: Send + Sync {
    async fn connect(&self, cfg: &WireguardConfig) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
    async fn is_active(&self) -> Result<bool>;
}

/// Mounts/unmounts SMB drives. Linux impl uses GVfs by default, kernel `mount.cifs` opt-in (ADR-0005).
#[async_trait]
pub trait MountBackend: Send + Sync {
    async fn mount(&self, drive: &Drive) -> Result<()>;
    async fn unmount(&self, drive: &Drive) -> Result<()>;
    /// Ids of drives currently mounted by us.
    async fn mounted(&self) -> Result<Vec<String>>;
}

/// Reports how a host can be reached right now.
#[async_trait]
pub trait Reachability: Send + Sync {
    async fn reach(&self, host: &Host) -> Reach;
}

/// Stores secrets (SSO tokens, the wg private key, SMB credentials) in the OS keyring.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn delete(&self, key: &str) -> Result<()>;
}

/// In-memory backends for testing orchestration anywhere. Reachability follows the VPN state: a host is
/// reachable via `Vpn` when the tunnel is up, else `Unreachable` (mirrors the away-without-Access case).
#[derive(Default)]
pub struct StubBackend {
    vpn_active: AtomicBool,
    mounts: Mutex<Vec<String>>,
    secrets: Mutex<HashMap<String, String>>,
}

impl StubBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VpnBackend for StubBackend {
    async fn connect(&self, _cfg: &WireguardConfig) -> Result<()> {
        self.vpn_active.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn disconnect(&self) -> Result<()> {
        self.vpn_active.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn is_active(&self) -> Result<bool> {
        Ok(self.vpn_active.load(Ordering::SeqCst))
    }
}

#[async_trait]
impl MountBackend for StubBackend {
    async fn mount(&self, drive: &Drive) -> Result<()> {
        let mut m = self.mounts.lock().unwrap();
        if !m.contains(&drive.id) {
            m.push(drive.id.clone());
        }
        Ok(())
    }
    async fn unmount(&self, drive: &Drive) -> Result<()> {
        self.mounts.lock().unwrap().retain(|d| d != &drive.id);
        Ok(())
    }
    async fn mounted(&self) -> Result<Vec<String>> {
        Ok(self.mounts.lock().unwrap().clone())
    }
}

#[async_trait]
impl Reachability for StubBackend {
    async fn reach(&self, _host: &Host) -> Reach {
        if self.vpn_active.load(Ordering::SeqCst) {
            Reach::Vpn
        } else {
            Reach::Unreachable
        }
    }
}

#[async_trait]
impl SecretStore for StubBackend {
    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.secrets.lock().unwrap().insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.secrets.lock().unwrap().get(key).cloned())
    }
    async fn delete(&self, key: &str) -> Result<()> {
        self.secrets.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host { console_id: "c1".into(), name: "Home".into(), wan_ip: None }
    }
    fn wg() -> WireguardConfig {
        WireguardConfig {
            server_public_key: "k".into(),
            endpoint: "1.2.3.4:51820".into(),
            allowed_ips: vec!["10.0.0.0/8".into()],
            preshared_key: None,
            persistent_keepalive: Some(25),
            address: vec!["10.2.0.5/32".into()],
            dns: vec![],
        }
    }

    #[tokio::test]
    async fn stub_vpn_toggles_and_gates_reachability() {
        let b = StubBackend::new();
        assert!(!b.is_active().await.unwrap());
        assert_eq!(b.reach(&host()).await, Reach::Unreachable);

        b.connect(&wg()).await.unwrap();
        assert!(b.is_active().await.unwrap());
        assert_eq!(b.reach(&host()).await, Reach::Vpn);

        b.disconnect().await.unwrap();
        assert_eq!(b.reach(&host()).await, Reach::Unreachable);
    }

    #[tokio::test]
    async fn stub_mounts_are_idempotent() {
        let b = StubBackend::new();
        let d = Drive { id: "d1".into(), name: "Design".into(), share: None, encrypted: false };
        b.mount(&d).await.unwrap();
        b.mount(&d).await.unwrap();
        assert_eq!(b.mounted().await.unwrap(), vec!["d1".to_string()]);
        b.unmount(&d).await.unwrap();
        assert!(b.mounted().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stub_secret_store_roundtrips() {
        let b = StubBackend::new();
        assert_eq!(b.get("token").await.unwrap(), None);
        b.set("token", "abc").await.unwrap();
        assert_eq!(b.get("token").await.unwrap().as_deref(), Some("abc"));
        b.delete("token").await.unwrap();
        assert_eq!(b.get("token").await.unwrap(), None);
    }
}
