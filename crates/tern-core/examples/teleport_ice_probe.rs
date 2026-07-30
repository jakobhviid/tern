//! Live probe of the Teleport ICE candidate gathering (ADR-0016 stage 4): print the machine's host
//! candidates and query a public STUN server for the reflexive (public) candidate — validating the STUN
//! client end-to-end against a real server, no invite needed.
//!
//! Usage: `cargo run -p tern-core --example teleport_ice_probe [-- stun.cloudflare.com:3478]`

use tern_core::teleport::ice;
use tokio::net::UdpSocket;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let stun_server = std::env::args().nth(1).unwrap_or_else(|| "stun.cloudflare.com:3478".to_string());

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let port = socket.local_addr()?.port();
    println!("bound UDP socket on port {port}\n");

    println!("host candidates:");
    for c in ice::local_candidates(port) {
        println!("  [{}] {}", c.kind, c.addr);
    }

    println!("\nquerying STUN {stun_server} for reflexive candidate…");
    match ice::reflexive_candidate(&socket, &stun_server).await {
        Some(c) => println!("  [{}] {}  ✅ (our public address)", c.kind, c.addr),
        None => println!("  (no response — STUN server unreachable or blocked)"),
    }
    Ok(())
}
