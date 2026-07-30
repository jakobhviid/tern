//! Teleport ICE candidate gathering (ADR-0016 stage 4): **host** candidates from the machine's interfaces
//! and the **reflexive** (public) candidate via a STUN Binding request. The nomination that follows — the
//! console-driven master/slave Binding exchange with MESSAGE-INTEGRITY — is the live-socket stage added next.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use rand_core::RngCore;
use tokio::net::UdpSocket;

use super::{stun, Candidate};

/// A random 12-byte STUN transaction id.
fn transaction_id() -> [u8; 12] {
    let mut id = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut id);
    id
}

/// Whether `ip` is usable as a candidate: not loopback / link-local / unspecified / multicast (and, for
/// IPv6, not a unique-local address). IPv4 private (LAN) addresses ARE kept — they're valid on-LAN
/// candidates (the reference client relies on exactly this to reach a console on the same network).
pub fn is_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast())
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xffc0) == 0xfe80 // link-local
                || (seg0 & 0xfe00) == 0xfc00) // unique-local (ULA)
        }
    }
}

/// Host candidates: every routable, non-loopback interface address, advertised at `port`.
pub fn local_candidates(port: u16) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return out;
    };
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        let ip = iface.ip();
        if !is_routable(ip) {
            continue;
        }
        let addr = SocketAddr::new(ip, port).to_string();
        if seen.insert(addr.clone()) {
            out.push(Candidate { kind: "iface".into(), addr });
        }
    }
    out
}

/// Query `stun_server` (`host:port`) on `socket` for our reflexive (public) address → a `reflex` candidate.
/// Uses the same socket the tunnel will use, so the reflexive address maps to that socket's NAT binding.
pub async fn reflexive_candidate(socket: &UdpSocket, stun_server: &str) -> Option<Candidate> {
    let req = stun::binding_request(&transaction_id());
    socket.send_to(&req, stun_server).await.ok()?;
    let mut buf = [0u8; 512];
    let (n, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
        .await
        .ok()?
        .ok()?;
    let addr = stun::parse_xor_mapped_address(&buf[..n])?;
    Some(Candidate { kind: "reflex".into(), addr: addr.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routable_filter_keeps_lan_drops_special() {
        assert!(is_routable("192.168.1.1".parse().unwrap())); // LAN kept
        assert!(is_routable("8.8.8.8".parse().unwrap()));
        assert!(!is_routable("127.0.0.1".parse().unwrap()));
        assert!(!is_routable("169.254.1.1".parse().unwrap())); // link-local
        assert!(!is_routable("fe80::1".parse().unwrap()));
        assert!(!is_routable("fc00::1".parse().unwrap())); // ULA
    }

    #[test]
    fn local_candidates_are_iface_typed_at_the_port() {
        for c in local_candidates(51820) {
            assert_eq!(c.kind, "iface");
            assert!(c.addr.ends_with(":51820"), "unexpected addr {}", c.addr);
        }
    }
}
