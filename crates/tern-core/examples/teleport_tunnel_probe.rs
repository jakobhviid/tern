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
    println!("✓ CONNECT_RESPONSE: server key {}, tunnel {}/{}", resp.server_info.wg_pub_key, resp.server_info.tunnel_addr, resp.server_info.tunnel_mask);

    println!("awaiting nomination (answering the console's STUN probes)…");
    let nominated: SocketAddr = nomination::await_nomination(&socket, &stun_secret, std::time::Duration::from_secs(45))
        .await
        .ok_or_else(|| anyhow::anyhow!("no endpoint was nominated within 45s"))?;
    println!("✓ nominated endpoint {nominated}");

    let wg_config = resp.to_wireguard_config(&kp, &nominated.to_string());
    let tunnel_addr: IpAddr = resp.server_info.tunnel_addr.parse()?;
    println!("bringing up {IFACE} with address {tunnel_addr}…");
    let tunnel = Tunnel::start(socket, nominated, &wg_config, IFACE).await?;
    run("ip", &["link", "set", IFACE, "up"]);

    // Pick a peer to ping through the tunnel. IPv6 overlay: the console is the low host on the /120 (…::1),
    // reachable via the connected route — no extra route needed. IPv4 overlay: route the console's /24 LAN
    // and aim at its gateway (.1).
    let (gateway, ping) = match tunnel_addr {
        IpAddr::V6(v6) => {
            let mut seg = v6.segments();
            seg[7] = 1; // …:1c00:1 — the overlay peer
            (IpAddr::V6(seg.into()), "ping")
        }
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let lan = format!("{}.{}.{}.0/24", o[0], o[1], o[2]);
            run("ip", &["route", "replace", &lan, "dev", IFACE]);
            println!("routed {lan} via {IFACE}");
            (IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 1)), "ping")
        }
    };
    println!("pinging peer {gateway} through the tunnel (give the handshake a moment)…");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let ok = Command::new(ping).args(["-c", "3", "-W", "3", &gateway.to_string()]).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        println!("\nRESULT: ✅ WireGuard handshake + data plane WORK — reached {gateway} through the Teleport tunnel.");
    } else {
        println!("\nRESULT: ⚠ tunnel is up but {gateway} didn't answer (it may block ICMP). Try another LAN host.");
    }

    println!("tunnel running on {} ({}). Ctrl-C to disconnect.", tunnel.interface, tunnel.address);
    tokio::signal::ctrl_c().await?;
    tunnel.stop().await;
    println!("disconnected.");
    Ok(())
}

fn run(cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("  `{cmd} {}` exited {s}", args.join(" ")),
        Err(e) => eprintln!("  `{cmd} {}` failed: {e}", args.join(" ")),
    }
}
