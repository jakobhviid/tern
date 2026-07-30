//! Teleport VPN backend (ADR-0016): the real [`TeleportVpn`] implementation for Linux. `redeem` is pure
//! signaling (broker pairing); `up` runs the in-process userspace-WireGuard/TUN data plane
//! ([`tern_core::teleport::establish`]) and configures the `tern0` interface with **iproute2** — the
//! in-process netlink address step is denied under SELinux even as root, whereas `ip` runs in its own
//! context and carries the daemon's `CAP_NET_ADMIN`. Bringing the tunnel up therefore needs that capability
//! (granted once via `setcap` on the daemon).

use async_trait::async_trait;
use tokio::sync::Mutex;

use tern_core::backend::TeleportVpn;
use tern_core::teleport::{self, dataplane::Tunnel, Broker, Invite, Session};
use tern_core::Result;

use crate::cmd;

/// The TUN interface name for the Teleport tunnel.
const IFACE: &str = "tern0";

/// Runs the in-process Teleport data plane and owns the live tunnel.
pub struct TeleportVpnBackend {
    broker: Broker,
    tunnel: Mutex<Option<Tunnel>>,
}

impl TeleportVpnBackend {
    pub fn new() -> Self {
        Self { broker: Broker::new(), tunnel: Mutex::new(None) }
    }
}

impl Default for TeleportVpnBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TeleportVpn for TeleportVpnBackend {
    async fn redeem(&self, invite: &Invite) -> Result<Session> {
        self.broker.pair(invite, "tern").await
    }

    async fn up(&self, session: &Session) -> Result<()> {
        // Replace any tunnel already up (idempotent reconnect).
        self.down().await?;
        let tunnel = teleport::establish(&self.broker, session, IFACE).await?;

        // Configure the interface with iproute2. Order matters for an IPv6 ULA overlay: link up first, then
        // clear the per-interface `disable_ipv6` (fresh TUN interfaces reject IPv6 addresses otherwise), then
        // assign the address the console gave us.
        cmd::run("ip", &["link", "set", IFACE, "up"]).await?;
        if tunnel.address.is_ipv6() {
            let _ = cmd::run("sysctl", &["-w", &format!("net.ipv6.conf.{IFACE}.disable_ipv6=0")]).await;
        }
        let cidr = format!("{}/{}", tunnel.address, tunnel.prefix);
        cmd::run("ip", &["addr", "add", &cidr, "dev", IFACE]).await?;

        *self.tunnel.lock().await = Some(tunnel);
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        if let Some(tunnel) = self.tunnel.lock().await.take() {
            tunnel.stop().await;
        }
        // The TUN interface disappears when its fd closes; delete defensively in case a prior run leaked it.
        let _ = cmd::status_ok("ip", &["link", "delete", IFACE]).await;
        Ok(())
    }

    async fn is_up(&self) -> Result<bool> {
        Ok(self.tunnel.lock().await.is_some())
    }
}
