//! LIVE end-to-end probe of the **whole** Teleport path (ADR-0016 stages ③–⑤): pair (or reuse a saved
//! session) → fetch ICE → gather candidates → CONNECT → answer the console's nomination → bring up a TUN
//! device and run userspace WireGuard over the ICE socket → route the console's LAN into it → ping the
//! gateway. This is the proof the data plane works against a real console.
//!
//! Needs CAP_NET_ADMIN to create the TUN and add the route, so run it privileged:
//!   cargo build -p tern-core --example teleport_tunnel_probe
//!   sudo ./target/debug/examples/teleport_tunnel_probe <teleport.ui.link invite | path/to/session.json>
//!
//! Passing a `teleport.ui.link` invite consumes its single-use pairing and saves the session next to it;
//! passing a saved session JSON path reuses it (no invite needed). Ctrl-C tears the tunnel down.

use std::net::{IpAddr, SocketAddr};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tern_core::teleport::{dataplane::Tunnel, ice, new_stun_secret, nomination, Broker, Invite, Session};
use tern_core::wg;
use tokio::net::UdpSocket;

const IFACE: &str = "tern0";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1).expect("pass a teleport.ui.link invite or a saved session JSON path");
    let broker = Broker::new();

    // Either redeem a fresh invite (consumes it, saves the session) or reuse a saved session.
    let session: Session = match Invite::parse(&arg) {
        Ok(invite) => {
            println!("pairing invite {} (this consumes it)…", invite.id);
            let s = broker.pair(&invite, "tern").await?;
            let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
            let path = format!("{dir}/tern-teleport-session.json");
            std::fs::write(&path, serde_json::to_string(&s)?)?;
            println!("✓ paired; session saved → {path}");
            s
        }
        Err(_) => {
            println!("reusing saved session {arg}");
            serde_json::from_str(&std::fs::read_to_string(&arg)?)?
        }
    };

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let port = socket.local_addr()?.port();
    let mut local = ice::local_candidates(port);
    println!("bound udp :{port}; {} host candidate(s)", local.len());

    let iceconf = broker.fetch_ice(&session).await?;
    let stun_server = iceconf
        .iter()
        .flat_map(|s| &s.urls)
        .find_map(|u| u.strip_prefix("stun:").map(|h| h.split('?').next().unwrap_or(h).to_string()))
        .unwrap_or_else(|| "stun.cloudflare.com:3478".to_string());
    if let Some(reflex) = ice::reflexive_candidate(&socket, &stun_server).await {
        println!("✓ reflexive candidate {}", reflex.addr);
        local.push(reflex);
    }

    let kp = wg::generate_keypair();
    let stun_secret = new_stun_secret();
    println!("connecting (offer: {} candidate(s))…", local.len());
    let resp = broker.connect(&session, &kp.public, &stun_secret, "tern", &local, &iceconf).await?;
    if resp.server_info.wg_pub_key.is_empty() || resp.server_info.tunnel_addr.is_empty() {
        anyhow::bail!(
            "console returned an EMPTY CONNECT_RESPONSE — a previous connection on this session is likely \
             still active. Wait ~2 min for it to expire and retry, or pass a fresh teleport.ui.link invite."
        );
    }
    let si = &resp.server_info;
    println!("✓ CONNECT_RESPONSE:");
    println!("    server key : {}", si.wg_pub_key);
    println!("    our tunnel : {}/{}", si.tunnel_addr, si.tunnel_mask);
    println!("    client_ip  : {}", if resp.client_ip.is_empty() { "(none)".into() } else { resp.client_ip.clone() });
    println!("    dns        : {:?}", resp.dns_addrs);
    println!("    udp echo   : {}:{}", if si.udp_echo_addr.is_empty() { "(none)".into() } else { si.udp_echo_addr.clone() }, si.udp_echo_port);

    println!("awaiting nomination (answering the console's STUN probes)…");
    let nominated: SocketAddr = nomination::await_nomination(&socket, &stun_secret, Duration::from_secs(45))
        .await
        .ok_or_else(|| anyhow::anyhow!("no endpoint was nominated within 45s"))?;
    println!("✓ nominated endpoint {nominated}");

    let wg_config = resp.to_wireguard_config(&kp, &nominated.to_string());
    println!("bringing up {IFACE} with address {}…", si.tunnel_addr);
    let tunnel = Tunnel::start(socket, nominated, &wg_config, IFACE, &stun_secret).await?;

    // Address + up are applied here (iproute2), not inside the library. Order matters: bring the link up,
    // then (for an IPv6 ULA overlay) clear the per-interface `disable_ipv6` — fresh TUN interfaces on this
    // host come up with IPv6 disabled, which rejects the address — then assign it.
    run("ip", &["link", "set", IFACE, "up"]);
    if tunnel.address.is_ipv6() {
        run("sysctl", &["-w", &format!("net.ipv6.conf.{IFACE}.disable_ipv6=0")]);
    }
    let cidr = format!("{}/{}", tunnel.address, tunnel.prefix);
    run("ip", &["addr", "add", &cidr, "dev", IFACE]);
    // A v4 client_ip (if the console gave one) goes on too, so v4 targets have a source address.
    if let Ok(v4) = resp.client_ip.parse::<IpAddr>() {
        run("ip", &["addr", "add", &format!("{v4}/32"), "dev", IFACE]);
    }
    println!("configured {IFACE} = {cidr}{}", if resp.client_ip.is_empty() { String::new() } else { format!(" + {}", resp.client_ip) });

    // Wait for the WireGuard handshake (polling the pump's live stats), then run the console's own health
    // check — a UDP echo through the tunnel — routing the echo target via the interface first.
    print!("waiting for the WireGuard handshake");
    let mut handshook = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        print!(".");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        if tunnel.stats.handshake.load(Ordering::Relaxed) {
            handshook = true;
            break;
        }
    }
    println!("\nhandshake: {}", if handshook { "✓ completed" } else { "✗ NOT completed" });

    let mut echo_ok = false;
    if let Ok(echo_dst) = si.udp_echo_addr.parse::<IpAddr>() {
        run("ip", &["route", "replace", &si.udp_echo_addr, "dev", IFACE]);
        let src: IpAddr = if echo_dst.is_ipv6() { tunnel.address } else { resp.client_ip.parse().unwrap_or(tunnel.address) };
        echo_ok = udp_echo(src, SocketAddr::new(echo_dst, si.udp_echo_port)).await;
        println!("udp echo {}:{} → {}", si.udp_echo_addr, si.udp_echo_port, if echo_ok { "✓ replied" } else { "✗ no reply" });
    }

    // Also try ICMP to the overlay peer (…::1) as a secondary signal.
    if let IpAddr::V6(v6) = tunnel.address {
        let mut seg = v6.segments();
        seg[7] = 1;
        let peer = IpAddr::V6(seg.into());
        let ping_ok = Command::new("ping").args(["-c", "2", "-W", "2", &peer.to_string()]).status().map(|s| s.success()).unwrap_or(false);
        println!("icmp {peer} → {}", if ping_ok { "✓ replied" } else { "✗ no reply" });
    }

    let s = &tunnel.stats;
    println!(
        "stats: net_in={} (stun {}), net_out={}, tun_in={}, tun_out={}, tx={}B rx={}B",
        s.net_in.load(Ordering::Relaxed), s.net_in_stun.load(Ordering::Relaxed), s.net_out.load(Ordering::Relaxed),
        s.tun_in.load(Ordering::Relaxed), s.tun_out.load(Ordering::Relaxed),
        s.tx_bytes.load(Ordering::Relaxed), s.rx_bytes.load(Ordering::Relaxed)
    );

    let data_plane_works = handshook && s.rx_bytes.load(Ordering::Relaxed) > 0;
    if echo_ok || data_plane_works {
        println!("\nRESULT: ✅ Teleport tunnel WORKS end-to-end (handshake + return traffic through the data plane).");
    } else if handshook {
        println!("\nRESULT: ◐ handshake completed but no return traffic — likely a routing/target issue, not the crypto.");
    } else {
        println!("\nRESULT: ✗ no WireGuard handshake — the pump sent to {nominated} but got nothing back (path/endpoint).");
    }

    tunnel.stop().await;
    println!("tunnel torn down; {IFACE} removed.");
    Ok(())
}

/// Send a datagram to the console's UDP-echo server through the tunnel (source-bound to our overlay address)
/// and report whether it echoes back within a short window — the console's built-in tunnel health check.
async fn udp_echo(src: IpAddr, dst: SocketAddr) -> bool {
    let Ok(sock) = tokio::net::UdpSocket::bind(SocketAddr::new(src, 0)).await else { return false };
    if sock.send_to(b"tern-echo", dst).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    matches!(tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf)).await, Ok(Ok((n, _))) if n > 0)
}

fn run(cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("  `{cmd} {}` exited {s}", args.join(" ")),
        Err(e) => eprintln!("  `{cmd} {}` failed: {e}", args.join(" ")),
    }
}
