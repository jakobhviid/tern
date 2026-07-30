//! Teleport data plane (ADR-0016 stage 5): userspace WireGuard over the ICE-nominated UDP socket, bridged to
//! a TUN device. `boringtun` runs the Noise IK handshake + transport crypto; we shuttle plaintext IP packets
//! between the TUN interface and the console. The **same** socket that carried ICE/nomination now carries
//! WireGuard transport to the nominated endpoint (its NAT binding is already pinned to the peer path).
//!
//! Creating the TUN device needs `CAP_NET_ADMIN` — granted once via `setcap` on the daemon (ADR-0016); this
//! module never execs a privileged helper. Routing (which traffic enters the tunnel) is layered on top by the
//! backend; here we only own the interface address, the crypto, and the packet pump.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tun_rs::{AsyncDevice, DeviceBuilder};

use super::stun;
use crate::error::Error;
use crate::Result;
use crate::model::WireguardConfig;

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;
/// WireGuard's standard tunnel MTU (1500 − 80 bytes of outer IPv6+UDP+WG overhead).
const TUN_MTU: u16 = 1420;
/// How often to service boringtun's timers (handshake retries, keepalives, expiry).
const TIMER_TICK: Duration = Duration::from_millis(250);

/// Decode a base64 WireGuard key into its 32 raw bytes.
fn key32(b64: &str) -> Result<[u8; 32]> {
    B64.decode(b64)
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok())
        .ok_or_else(|| Error::InvalidConfig("a WireGuard key was not valid 32-byte base64".into()))
}

/// The interface address (`ip/prefix`) from a WireGuard config's `Address` list — the first parseable entry.
/// Teleport assigns a single tunnel address, either IPv4 or an IPv6 ULA (fd37::/… overlay), so we accept
/// whichever family the console gave us and default the prefix to a host route for its family.
fn interface_addr(cfg: &WireguardConfig) -> Result<(IpAddr, u8)> {
    cfg.address
        .iter()
        .find_map(|entry| {
            let (ip, prefix) = entry.split_once('/').unwrap_or((entry.as_str(), ""));
            let ip: IpAddr = ip.trim().parse().ok()?;
            let host_prefix = if ip.is_ipv6() { 128 } else { 32 };
            let prefix = prefix.trim().parse().unwrap_or(host_prefix).min(host_prefix);
            Some((ip, prefix))
        })
        .ok_or_else(|| Error::InvalidConfig("the config has no tunnel address".into()))
}

/// Live counters for the tunnel, updated by the pump — enough to tell "handshake done + bytes flowing" from
/// "stuck". The GUI/daemon can surface throughput from the same struct.
#[derive(Default)]
pub struct Stats {
    /// Datagrams received on the ICE socket.
    pub net_in: AtomicU64,
    /// Of those, STUN messages (the console's nomination / consent keepalives).
    pub net_in_stun: AtomicU64,
    /// Datagrams sent to the peer (handshake, keepalive, transport).
    pub net_out: AtomicU64,
    /// Plaintext packets read from the TUN (outbound app traffic).
    pub tun_in: AtomicU64,
    /// Decrypted packets delivered to the TUN (inbound app traffic).
    pub tun_out: AtomicU64,
    /// Whether a WireGuard handshake has completed at least once.
    pub handshake: AtomicBool,
    /// boringtun's transport byte counters (encrypted payload).
    pub tx_bytes: AtomicU64,
    pub rx_bytes: AtomicU64,
}

/// A running Teleport tunnel: a background task pumping packets between the socket and the TUN device, plus
/// the handle to stop it. Dropping the [`Tunnel`] without [`Tunnel::stop`] aborts the pump (and the OS tears
/// the TUN device down).
pub struct Tunnel {
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    /// Live throughput/handshake counters (shared with the pump).
    pub stats: Arc<Stats>,
    /// The TUN interface name (e.g. `tern0`) — the backend routes traffic onto this.
    pub interface: String,
    /// The tunnel address the console assigned us (source address for tunneled traffic). The caller applies
    /// it (`ip addr add`) and brings the link up — we deliberately don't configure addressing from inside the
    /// library (it needs privilege that iproute2 already has, and keeps routing policy in one place).
    pub address: IpAddr,
    /// The prefix length that goes with [`Tunnel::address`].
    pub prefix: u8,
}

impl Tunnel {
    /// Bring up a TUN device configured from `cfg`, then drive userspace WireGuard over `socket` to the
    /// ICE-nominated `endpoint`. `socket` must already be the one nomination pinned to the peer path.
    pub async fn start(
        socket: UdpSocket,
        endpoint: SocketAddr,
        cfg: &WireguardConfig,
        if_name: &str,
    ) -> Result<Tunnel> {
        let private = cfg
            .client_private_key
            .as_deref()
            .ok_or_else(|| Error::InvalidConfig("the config has no private key".into()))?;
        let static_private = StaticSecret::from(key32(private)?);
        let peer_public = PublicKey::from(key32(&cfg.server_public_key)?);
        let preshared = match cfg.preshared_key.as_deref() {
            Some(psk) => Some(key32(psk)?),
            None => None,
        };
        let keepalive = cfg.persistent_keepalive.unwrap_or(25);
        let tunn = Tunn::new(static_private, peer_public, preshared, Some(keepalive), 0, None);

        let (address, prefix) = interface_addr(cfg)?;
        // Create the interface bare (just name + MTU). Addressing/up/routes are the caller's job via iproute2
        // — we don't do the netlink address dance in-process (it's the privileged step that iproute2 already
        // handles cleanly, and it keeps all routing policy in one place).
        let device = DeviceBuilder::new()
            .name(if_name)
            .mtu(TUN_MTU)
            .build_async()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => Error::PrivilegeRequired,
                _ => Error::Other(anyhow::anyhow!("teleport: could not create TUN {if_name}: {e}")),
            })?;

        let stop = Arc::new(Notify::new());
        let stats = Arc::new(Stats::default());
        let task = tokio::spawn(pump(tunn, Arc::new(socket), Arc::new(device), endpoint, stop.clone(), stats.clone()));
        Ok(Tunnel { stop, task, stats, interface: if_name.to_string(), address, prefix })
    }

    /// Stop the tunnel: signal the pump to exit and wait for it (which drops the TUN device).
    pub async fn stop(self) {
        self.stop.notify_waiters();
        let _ = self.task.await;
    }
}

/// The packet pump: one task owning the boringtun state machine, selecting over the encrypted network side,
/// the plaintext TUN side, boringtun's timers, and the stop signal. boringtun requires `&mut self` on
/// encapsulate/decapsulate/timers, so keeping it in a single task avoids locking.
async fn pump(
    mut tunn: Tunn,
    socket: Arc<UdpSocket>,
    device: Arc<AsyncDevice>,
    endpoint: SocketAddr,
    stop: Arc<Notify>,
    stats: Arc<Stats>,
) {
    let mut net_buf = [0u8; 1600]; // ciphertext in from the socket
    let mut tun_buf = [0u8; 1600]; // plaintext in from the TUN
    let mut out = [0u8; 1600]; // scratch for boringtun output (both directions)

    // Kick the handshake off proactively so keepalives flow even before the first TUN packet.
    if let TunnResult::WriteToNetwork(p) = tunn.format_handshake_initiation(&mut out, false) {
        stats.net_out.fetch_add(1, Ordering::Relaxed);
        let _ = socket.send_to(p, endpoint).await;
    }

    let mut ticker = tokio::time::interval(TIMER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = stop.notified() => break,

            _ = ticker.tick() => {
                if let TunnResult::WriteToNetwork(p) = tunn.update_timers(&mut out) {
                    stats.net_out.fetch_add(1, Ordering::Relaxed);
                    let _ = socket.send_to(p, endpoint).await;
                }
                // Snapshot boringtun's view: a completed handshake + byte counters.
                let (handshake, tx, rx, _, _) = tunn.stats();
                if handshake.is_some() {
                    stats.handshake.store(true, Ordering::Relaxed);
                }
                stats.tx_bytes.store(tx as u64, Ordering::Relaxed);
                stats.rx_bytes.store(rx as u64, Ordering::Relaxed);
            }

            recv = socket.recv_from(&mut net_buf) => {
                let Ok((n, from)) = recv else { continue };
                stats.net_in.fetch_add(1, Ordering::Relaxed);
                // The console may still send authenticated STUN keepalives on this socket after nomination;
                // those aren't WireGuard, so don't feed them to boringtun.
                if stun::is_stun(&net_buf[..n]) {
                    stats.net_in_stun.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // decapsulate can ask us to write more to the network (queued handshake/keepalive); the
                // contract is to repeat with an empty datagram until it stops.
                let mut datagram: &[u8] = &net_buf[..n];
                loop {
                    match tunn.decapsulate(Some(from.ip()), datagram, &mut out) {
                        TunnResult::WriteToNetwork(p) => {
                            stats.net_out.fetch_add(1, Ordering::Relaxed);
                            let _ = socket.send_to(p, endpoint).await;
                            datagram = &[];
                        }
                        TunnResult::WriteToTunnelV4(p, _) | TunnResult::WriteToTunnelV6(p, _) => {
                            stats.tun_out.fetch_add(1, Ordering::Relaxed);
                            let _ = device.send(p).await;
                            break;
                        }
                        _ => break,
                    }
                }
            }

            recv = device.recv(&mut tun_buf) => {
                let Ok(n) = recv else { continue };
                stats.tun_in.fetch_add(1, Ordering::Relaxed);
                if let TunnResult::WriteToNetwork(p) = tunn.encapsulate(&tun_buf[..n], &mut out) {
                    stats.net_out.fetch_add(1, Ordering::Relaxed);
                    let _ = socket.send_to(p, endpoint).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(addr: &[&str]) -> WireguardConfig {
        WireguardConfig {
            server_public_key: B64.encode([1u8; 32]),
            endpoint: "1.2.3.4:51820".into(),
            allowed_ips: vec![],
            preshared_key: None,
            persistent_keepalive: Some(25),
            address: addr.iter().map(|s| s.to_string()).collect(),
            dns: vec![],
            client_private_key: Some(B64.encode([2u8; 32])),
        }
    }

    #[test]
    fn decodes_a_valid_wireguard_key() {
        assert_eq!(key32(&B64.encode([7u8; 32])).unwrap(), [7u8; 32]);
        assert!(key32("not-base64!!").is_err());
        assert!(key32(&B64.encode([0u8; 16])).is_err()); // wrong length
    }

    #[test]
    fn parses_the_interface_address_and_prefix() {
        let v4: IpAddr = "192.168.2.7".parse().unwrap();
        assert_eq!(interface_addr(&cfg_with(&["192.168.2.7/32"])).unwrap(), (v4, 32));
        assert_eq!(interface_addr(&cfg_with(&["10.0.0.5"])).unwrap(), ("10.0.0.5".parse().unwrap(), 32));
        // IPv6 ULA tunnel address (what real UniFi Teleport assigns) — the /120 overlay prefix is kept.
        assert_eq!(
            interface_addr(&cfg_with(&["fd37:5753:430c:4aee:b66a:e44d:1c00:2/120"])).unwrap(),
            ("fd37:5753:430c:4aee:b66a:e44d:1c00:2".parse().unwrap(), 120)
        );
        // A bare IPv6 with no prefix defaults to a /128 host route.
        assert_eq!(interface_addr(&cfg_with(&["fd00::1"])).unwrap(), ("fd00::1".parse().unwrap(), 128));
        assert!(interface_addr(&cfg_with(&[])).is_err());
    }
}
