//! LIVE end-to-end probe of the Teleport control plane (ADR-0016 stages ③–④ connect): redeem an invite →
//! pair → save the session → fetch ICE → gather candidates → CONNECT → print the console's CONNECT_RESPONSE.
//!
//! **Consumes the invite's pairing capability** (single-use). The resulting session is saved so the later
//! nomination + WireGuard stages don't need a fresh invite.
//!
//! Usage: `cargo run -p tern-core --example teleport_connect_probe -- <teleport.ui.link invite>`

use tern_core::teleport::{ice, new_stun_secret, Broker, Invite};
use tern_core::wg;
use tokio::net::UdpSocket;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1).expect("pass a teleport.ui.link invite (URL or UUID)");
    let invite = Invite::parse(&arg)?;
    println!("invite {}", invite.id);

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let port = socket.local_addr()?.port();
    let mut local = ice::local_candidates(port);
    println!("bound udp :{port}; {} host candidate(s)", local.len());

    let broker = Broker::new();
    println!("pairing (this consumes the invite)…");
    let session = broker.pair(&invite, "tern").await?;
    println!("✓ paired");

    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{dir}/tern-teleport-session.json");
    std::fs::write(&path, serde_json::to_string(&session)?)?;
    println!("  session saved → {path} (reuse for nomination/WireGuard; no invite needed)");

    println!("fetching ICE configuration…");
    let iceconf = broker.fetch_ice(&session).await?;
    println!("✓ {} ICE server(s):", iceconf.len());
    for s in &iceconf {
        println!("    {:?}", s.urls);
    }

    // Reflexive candidate on the tunnel socket, via a STUN server from the ICE config (fallback Cloudflare).
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

    println!("\n=== CONNECT_RESPONSE ===");
    println!("server wg_pub_key : {}", resp.server_info.wg_pub_key);
    println!("tunnel address    : {}/{}", resp.server_info.tunnel_addr, resp.server_info.tunnel_mask);
    println!("client_ip / dns   : {} / {:?}", resp.client_ip, resp.dns_addrs);
    println!("server candidates ({}):", resp.server_info.peer_desc.candidates.len());
    for c in &resp.server_info.peer_desc.candidates {
        println!("    [{}] {}", c.kind, c.addr);
    }
    println!("\nRESULT: ✅ pair + ICE + connect all succeeded live — the Teleport control plane is validated");
    println!("end-to-end. Remaining: reply to the console's nomination + run WireGuard over the socket.");
    Ok(())
}
