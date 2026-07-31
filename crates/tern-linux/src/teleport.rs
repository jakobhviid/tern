//! Teleport VPN backend (ADR-0016): the real [`TeleportVpn`] implementation for Linux. `redeem` is pure
//! signaling (broker pairing); `up` runs the in-process userspace-WireGuard/TUN data plane
//! ([`tern_core::teleport::establish`]) and configures the `tern0` interface with **iproute2** — the
//! in-process netlink address step is denied under SELinux even as root, whereas `ip` runs in its own
//! context and carries the daemon's `CAP_NET_ADMIN`. Bringing the tunnel up therefore needs that capability
//! (granted once via `setcap` on the daemon).
//!
//! The addressing/routing was validated live against a real console: the console assigns a v6 ULA overlay
//! (`fd37::/…`) **and** a v4 `client_ip` on its LAN; the remote v4 subnets are reached *through* the tunnel,
//! while the ICE-nominated underlay endpoint's own subnet is the local transport path and must never be
//! routed into the TUN (that would loop the WireGuard packets).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};

use async_trait::async_trait;
use tokio::sync::Mutex;

use tern_core::backend::TeleportVpn;
use tern_core::teleport::{self, Broker, Connection, Invite, Session};
use tern_core::Result;

use crate::cmd;

/// The TUN interface name for the Teleport tunnel.
const IFACE: &str = "tern0";

/// One iproute2/sysctl step: the program, its args, and whether it must succeed (`ip` address/link/route) or
/// is best-effort (`sysctl` toggles that may legitimately be no-ops on some hosts).
struct Step {
    program: &'static str,
    args: Vec<String>,
    required: bool,
}

fn ip(args: &[&str]) -> Step {
    Step { program: "ip", args: args.iter().map(|s| s.to_string()).collect(), required: true }
}

fn sysctl(kv: String) -> Step {
    Step { program: "sysctl", args: vec!["-w".into(), kv], required: false }
}

/// The `/24` containing `a`, as an `ip route` CIDR.
fn slash24(a: Ipv4Addr) -> String {
    let o = a.octets();
    format!("{}.{}.{}.0/24", o[0], o[1], o[2])
}

/// The full command sequence to configure `tern0` from a connection's addressing. **Ordering matters**
/// (learned live): link up → clear per-interface `disable_ipv6` for a v6 overlay (fresh TUN interfaces reject
/// v6 addresses otherwise) → assign the v6 address → assign the v4 `client_ip` → loosen `rp_filter` so the
/// host doesn't drop the tunnel's return traffic → route the remote v4 `/24`s (from `client_ip` + `dns`),
/// **excluding** the underlay endpoint's `/24`. Pure + unit-tested so the order/exclusion can't regress.
fn configure_steps(address: IpAddr, prefix: u8, client_ip: Option<Ipv4Addr>, dns: &[IpAddr], endpoint: IpAddr) -> Vec<Step> {
    let mut steps = vec![ip(&["link", "set", IFACE, "up"])];
    if address.is_ipv6() {
        steps.push(sysctl(format!("net.ipv6.conf.{IFACE}.disable_ipv6=0")));
    }
    steps.push(ip(&["addr", "add", &format!("{address}/{prefix}"), "dev", IFACE]));
    if let Some(v4) = client_ip {
        steps.push(ip(&["addr", "add", &format!("{v4}/32"), "dev", IFACE]));
    }
    // Keep the host from dropping the tunnel's decrypted return traffic: loose reverse-path globally, off on
    // the tunnel interface. (Best-effort; needs CAP_NET_ADMIN, which the daemon has.)
    steps.push(sysctl("net.ipv4.conf.all.rp_filter=2".into()));
    steps.push(sysctl(format!("net.ipv4.conf.{IFACE}.rp_filter=0")));

    // Route the remote v4 subnets through the tunnel, never the underlay endpoint's own /24.
    let endpoint_net = match endpoint {
        IpAddr::V4(v4) => Some(slash24(v4)),
        _ => None,
    };
    let mut targets: Vec<Ipv4Addr> = client_ip.into_iter().collect();
    targets.extend(dns.iter().filter_map(|d| match d {
        IpAddr::V4(v4) => Some(*v4),
        _ => None,
    }));
    let mut seen: HashSet<String> = HashSet::new();
    for t in targets {
        let net = slash24(t);
        if Some(&net) == endpoint_net.as_ref() || !seen.insert(net.clone()) {
            continue;
        }
        steps.push(ip(&["route", "replace", &net, "dev", IFACE]));
    }
    steps
}

/// Run the required interface-setup steps in order (best-effort steps never fail the connect). A capability
/// failure surfaces as EPERM from netlink (`... Operation not permitted`); map it to the plain
/// [`tern_core::Error::PrivilegeRequired`] ("needs permission" → run setcap) rather than leaking `RTNETLINK`
/// jargon to the user.
async fn run_steps(steps: Vec<Step>) -> Result<()> {
    for step in steps {
        let argv: Vec<&str> = step.args.iter().map(String::as_str).collect();
        let res = cmd::run(step.program, &argv).await;
        if step.required {
            if let Err(e) = res {
                if e.to_string().contains("not permitted") {
                    return Err(tern_core::Error::PrivilegeRequired);
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Runs the in-process Teleport data plane and owns the live tunnel.
pub struct TeleportVpnBackend {
    broker: Broker,
    tunnel: Mutex<Option<teleport::dataplane::Tunnel>>,
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
        let conn: Connection = teleport::establish(&self.broker, session, IFACE).await?;

        // Configure the interface; if any required step fails, tear the *just-created* tunnel down (it isn't
        // stored yet, so stop it directly — otherwise the pump/TUN would leak) and report.
        let steps = configure_steps(conn.tunnel.address, conn.tunnel.prefix, conn.client_ip, &conn.dns, conn.endpoint.ip());
        if let Err(e) = run_steps(steps).await {
            conn.tunnel.stop().await;
            let _ = cmd::status_ok("ip", &["link", "delete", IFACE]).await;
            return Err(e);
        }

        // DNS through the tunnel (best-effort — needs systemd-resolved; connectivity by IP works without it).
        if !conn.dns.is_empty() {
            let mut args = vec!["dns".to_string(), IFACE.to_string()];
            args.extend(conn.dns.iter().map(|d| d.to_string()));
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let _ = cmd::run("resolvectl", &argv).await;
            let _ = cmd::run("resolvectl", &["domain", IFACE, "~."]).await;
        }

        *self.tunnel.lock().await = Some(conn.tunnel);
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        if let Some(tunnel) = self.tunnel.lock().await.take() {
            tunnel.stop().await;
        }
        // The TUN interface disappears when its fd closes (taking its addresses/routes with it); delete
        // defensively in case a prior run leaked it.
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

    fn progs(steps: &[Step]) -> Vec<(&str, Vec<&str>)> {
        steps.iter().map(|s| (s.program, s.args.iter().map(String::as_str).collect())).collect()
    }

    #[test]
    fn ipv6_overlay_with_v4_client_ip_and_routes() {
        // The live-confirmed shape: v6 tunnel + v4 client_ip 192.168.2.11, DNS 192.168.1.1, underlay on
        // 192.168.8.1 — route 192.168.2.0/24 + 192.168.1.0/24, but NOT 192.168.8.0/24.
        let steps = configure_steps(
            "fd37:5753:430c:4aee:b66a:e44d:1c00:2".parse().unwrap(),
            120,
            Some("192.168.2.11".parse().unwrap()),
            &["192.168.1.1".parse().unwrap()],
            "192.168.8.1".parse().unwrap(),
        );
        let p = progs(&steps);
        // Order: link up → disable_ipv6 → v6 addr → v4 addr → rp_filter×2 → routes.
        assert_eq!(p[0], ("ip", vec!["link", "set", IFACE, "up"]));
        assert_eq!(p[1].0, "sysctl");
        assert!(p[1].1[1].ends_with("disable_ipv6=0"));
        assert!(p[2].1.contains(&"fd37:5753:430c:4aee:b66a:e44d:1c00:2/120"));
        assert!(p[3].1.contains(&"192.168.2.11/32"));
        // Routes: both remote /24s present, the underlay /24 absent.
        let routes: Vec<&str> = steps
            .iter()
            .filter(|s| s.args.first().map(String::as_str) == Some("route"))
            .map(|s| s.args[2].as_str())
            .collect();
        assert!(routes.contains(&"192.168.2.0/24"));
        assert!(routes.contains(&"192.168.1.0/24"));
        assert!(!routes.contains(&"192.168.8.0/24"), "must not route the underlay endpoint's subnet");
    }

    #[test]
    fn ipv4_only_needs_no_disable_ipv6_step() {
        let steps = configure_steps("10.2.0.5".parse().unwrap(), 32, None, &[], "10.9.9.1".parse().unwrap());
        assert_eq!(steps[0].args.last().unwrap(), "up");
        assert!(!steps.iter().any(|s| s.program == "sysctl" && s.args[1].contains("disable_ipv6")));
        assert!(steps.iter().any(|s| s.args.first().map(String::as_str) == Some("addr") && s.args[2] == "10.2.0.5/32"));
    }

    fn sh(script: &str, required: bool) -> Step {
        Step { program: "sh", args: vec!["-c".into(), script.into()], required }
    }

    #[tokio::test]
    async fn a_not_permitted_step_maps_to_privilege_required() {
        // A required step failing with an EPERM-style message → the plain PrivilegeRequired, not RTNETLINK jargon.
        let steps = vec![sh("echo 'RTNETLINK answers: Operation not permitted' >&2; exit 1", true)];
        let err = run_steps(steps).await.unwrap_err();
        assert!(matches!(err, tern_core::Error::PrivilegeRequired));
    }

    #[tokio::test]
    async fn a_required_step_failure_that_isnt_a_permission_error_propagates() {
        let steps = vec![sh("echo 'some other failure' >&2; exit 1", true)];
        let err = run_steps(steps).await.unwrap_err();
        assert!(!matches!(err, tern_core::Error::PrivilegeRequired));
    }

    #[tokio::test]
    async fn best_effort_step_failures_do_not_fail_the_connect() {
        // A best-effort step (e.g. a sysctl no-op) may fail without aborting; a following required step still runs.
        let steps = vec![sh("exit 1", false), sh("exit 0", true)];
        assert!(run_steps(steps).await.is_ok());
    }

    #[test]
    fn a_route_matching_the_endpoint_subnet_is_skipped() {
        // client_ip shares the endpoint's /24 → no route for it (would loop the WireGuard underlay).
        let steps = configure_steps(
            "10.0.0.2".parse().unwrap(),
            32,
            Some("192.168.4.50".parse().unwrap()),
            &[],
            "192.168.4.1".parse().unwrap(),
        );
        assert!(!steps.iter().any(|s| s.args.first().map(String::as_str) == Some("route")));
    }
}
