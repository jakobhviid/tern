//! Teleport VPN backend (ADR-0016): the real [`TeleportVpn`] implementation for Linux. `redeem` is pure
//! signaling (broker pairing); `up` runs the in-process userspace-WireGuard/TUN data plane
//! ([`tern_core::teleport::establish`]) and configures the `tern0` interface with **iproute2** — the
//! in-process netlink address step is denied under SELinux even as root, whereas `ip` runs in its own
//! context and carries the daemon's `CAP_NET_ADMIN`. Bringing the tunnel up therefore needs that capability
//! (granted once via `setcap` on the daemon).

use std::net::IpAddr;

use async_trait::async_trait;
use tokio::sync::Mutex;

use tern_core::backend::TeleportVpn;
use tern_core::teleport::{self, dataplane::Tunnel, Broker, Invite, Session};
use tern_core::Result;

use crate::cmd;

/// The TUN interface name for the Teleport tunnel.
const IFACE: &str = "tern0";

/// One iproute2/sysctl step: the program, its args, and whether it must succeed (`ip` address/link) or is
/// best-effort (`sysctl` toggling `disable_ipv6`).
struct Step {
    program: &'static str,
    args: Vec<String>,
    required: bool,
}

/// The command sequence to configure `tern0` for `address/prefix`. **Ordering matters** (learned against a
/// live console): bring the link up, then for an IPv6 ULA overlay clear the per-interface `disable_ipv6`
/// (fresh TUN interfaces reject IPv6 addresses otherwise), then assign the address. Pure + unit-tested so the
/// order can't silently regress.
fn interface_setup(address: IpAddr, prefix: u8) -> Vec<Step> {
    let mut steps = vec![Step {
        program: "ip",
        args: vec!["link".into(), "set".into(), IFACE.into(), "up".into()],
        required: true,
    }];
    if address.is_ipv6() {
        steps.push(Step {
            program: "sysctl",
            args: vec!["-w".into(), format!("net.ipv6.conf.{IFACE}.disable_ipv6=0")],
            required: false,
        });
    }
    steps.push(Step {
        program: "ip",
        args: vec!["addr".into(), "add".into(), format!("{address}/{prefix}"), "dev".into(), IFACE.into()],
        required: true,
    });
    steps
}

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

        // Configure the interface with iproute2 (see interface_setup for the ordering rationale).
        for step in interface_setup(tunnel.address, tunnel.prefix) {
            let argv: Vec<&str> = step.args.iter().map(String::as_str).collect();
            let run = cmd::run(step.program, &argv).await;
            if step.required {
                run?;
            } // best-effort steps (disable_ipv6) may legitimately fail on hosts where it's already clear.
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_setup_brings_the_link_up_then_assigns_the_address() {
        let steps = interface_setup("10.2.0.5".parse().unwrap(), 32);
        assert_eq!(steps.len(), 2);
        assert_eq!((steps[0].program, steps[0].args.as_slice()), ("ip", ["link", "set", IFACE, "up"].map(String::from).as_slice()));
        assert_eq!(steps[1].program, "ip");
        assert_eq!(steps[1].args, ["addr", "add", "10.2.0.5/32", "dev", IFACE]);
        assert!(steps.iter().all(|s| s.required)); // no sysctl step for IPv4
    }

    #[test]
    fn ipv6_setup_clears_disable_ipv6_between_link_up_and_the_address() {
        let steps = interface_setup("fd37:5753:430c:4aee:b66a:e44d:1c00:2".parse().unwrap(), 120);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].args.last().unwrap(), "up"); // link up first
        // disable_ipv6 must come BEFORE the address, and be best-effort.
        assert_eq!(steps[1].program, "sysctl");
        assert!(steps[1].args[1].ends_with("disable_ipv6=0"));
        assert!(!steps[1].required);
        assert_eq!(steps[2].args[0], "addr"); // address last
        assert!(steps[2].args.contains(&"fd37:5753:430c:4aee:b66a:e44d:1c00:2/120".to_string()));
    }
}
