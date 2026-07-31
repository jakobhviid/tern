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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tern_core::teleport::{dataplane::Tunnel, ice, new_stun_secret, nomination, Broker, Invite, Session};
use tern_core::wg;
use tokio::net::UdpSocket;

const IFACE: &str = "tern0";
/// Where a paired session is saved for reuse — persistent across sudo sessions (unlike `/run/user/0`).
const SESSION_PATH: &str = "/tmp/tern-teleport-session.json";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1).expect("pass a teleport.ui.link invite or a saved session JSON path");
    let broker = Broker::new();

    // Either redeem a fresh invite (consumes it, saves the session) or reuse a saved session.
    let session: Session = match Invite::parse(&arg) {
        Ok(invite) => {
            println!("pairing invite {} (this consumes it)…", invite.id);
            let s = broker.pair(&invite, "tern").await?;
            // A fixed /tmp path (not $XDG_RUNTIME_DIR) — under sudo that's /run/user/0, which systemd wipes
            // when root's login session ends between calls, losing the reusable session.
            let path = SESSION_PATH;
            std::fs::write(path, serde_json::to_string(&s)?)?;
            let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
            println!("✓ paired; session saved → {path} (reuse it, no new invite needed)");
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

    // Route the remote v4 subnets through the tunnel. The console placed us at client_ip on its LAN and its
    // DNS sits on another subnet — both are reached *through* tern0. We must NOT route the underlay endpoint's
    // /24 (that's our local LAN + the WireGuard transport path) or packets would loop.
    let endpoint_net = match nominated.ip() {
        IpAddr::V4(v4) => Some(slash24(v4)),
        _ => None,
    };
    let mut v4_targets: Vec<Ipv4Addr> = Vec::new();
    if let Ok(c) = resp.client_ip.parse::<Ipv4Addr>() {
        v4_targets.push(c);
    }
    v4_targets.extend(resp.dns_addrs.iter().filter_map(|d| d.parse::<Ipv4Addr>().ok()));
    let mut routed: Vec<String> = Vec::new();
    for t in &v4_targets {
        let net = slash24(*t);
        if Some(&net) == endpoint_net.as_ref() || routed.contains(&net) {
            continue;
        }
        run("ip", &["route", "replace", &net, "dev", IFACE]);
        routed.push(net);
    }
    if !routed.is_empty() {
        println!("routed remote v4 subnets via {IFACE}: {}", routed.join(", "));
    }

    // The console's decrypted replies reach tern0 but can be dropped by the host's input path before the app
    // sees them — Bazzite runs firewalld (a fresh interface lands in the untrusted default zone) and rp_filter
    // may apply. Loosen both for tern0 so the probe measures genuine app connectivity. (Runtime-only; the
    // real backend will do this more surgically.)
    run("sysctl", &["-w", "net.ipv4.conf.all.rp_filter=2"]);
    run("sysctl", &["-w", &format!("net.ipv4.conf.{IFACE}.rp_filter=0")]);
    run("firewall-cmd", &["--zone=trusted", "--add-interface", IFACE]);

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

    // The real path is v4: ping the remote DNS server (a known live LAN host behind the console), sourced
    // from our client_ip so the console's cryptokey routing accepts it.
    let mut v4_ok = false;
    if let (Ok(src), Some(dst)) =
        (resp.client_ip.parse::<Ipv4Addr>(), resp.dns_addrs.iter().find_map(|d| d.parse::<Ipv4Addr>().ok()))
    {
        v4_ok = Command::new("ping")
            .args(["-c", "3", "-W", "3", "-I", &src.to_string(), &dst.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        println!("v4 ping {dst} (from {src}) → {}", if v4_ok { "✓ replied" } else { "✗ no reply" });
    }

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
    if let Ok(samples) = s.inbound_samples.lock() {
        if !samples.is_empty() {
            println!("decrypted inbound packets (what the console actually sent back):");
            for line in samples.iter() {
                println!("    {line}");
            }
        }
    }

    // Host-side diagnostics: is our address on the interface, which way does the kernel route the target,
    // and did the interface actually receive the injected replies (RX counters)?
    println!("\n--- host diagnostics ---");
    sh("ip", &["addr", "show", IFACE]);
    sh("ip", &["route", "get", &resp.dns_addrs.first().cloned().unwrap_or_else(|| "192.168.1.1".into())]);
    sh("ip", &["-s", "link", "show", IFACE]);
    // ICMP input counters (header + values): if InEchoReps grew by ~3, the kernel received the replies and
    // it's a ping-socket issue; if not, they were dropped before ICMP (routing/martian).
    sh("sh", &["-c", "grep '^Icmp' /proc/net/snmp"]);

    let returned = s.rx_bytes.load(Ordering::Relaxed) > 0 || s.tun_out.load(Ordering::Relaxed) > 0;
    if v4_ok || echo_ok || (handshook && returned) {
        println!("\nRESULT: ✅ Teleport tunnel WORKS end-to-end (handshake + return traffic through the data plane).");
    } else if handshook {
        println!("\nRESULT: ◐ handshake completed but no return traffic — routing/target, not the crypto. Try a different remote host, or check the console accepts our source address (cryptokey routing).");
    } else {
        println!("\nRESULT: ✗ no WireGuard handshake — the pump sent to {nominated} but got nothing back (path/endpoint).");
    }

    tunnel.stop().await;
    println!("tunnel torn down; {IFACE} removed.");
    Ok(())
}

/// The `/24` containing `a`, as an `ip route` CIDR string.
fn slash24(a: Ipv4Addr) -> String {
    let o = a.octets();
    format!("{}.{}.{}.0/24", o[0], o[1], o[2])
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

/// Run a diagnostic command and print its output (indented), for the host-diagnostics section.
fn sh(cmd: &str, args: &[&str]) {
    println!("$ {cmd} {}", args.join(" "));
    match Command::new(cmd).args(args).output() {
        Ok(o) => {
            for line in String::from_utf8_lossy(&o.stdout).lines().chain(String::from_utf8_lossy(&o.stderr).lines()) {
                println!("    {line}");
            }
        }
        Err(e) => println!("    (failed: {e})"),
    }
}

fn run(cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("  `{cmd} {}` exited {s}", args.join(" ")),
        Err(e) => eprintln!("  `{cmd} {}` failed: {e}", args.join(" ")),
    }
}
