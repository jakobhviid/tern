//! Live probe of the Teleport signaling foundation (ADR-0016 stages ①–②) against the real broker.
//!
//! Parses a `teleport.ui.link` invite, derives the broker request token with our `secret_to_token`, and
//! calls the token-only `/metadata` endpoint. A `200` with console info means our invite parsing + token
//! derivation + broker transport are correct against the live service — no ICE/WireGuard needed to check.
//!
//! Usage: `cargo run -p tern-core --example teleport_probe -- <teleport.ui.link URL or UUID>`
//! (Read-only; does not consume the invite's pairing capability.)

use tern_core::teleport::{Invite, BROKER_BASE};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1).expect("pass a teleport.ui.link invite (URL or UUID)");

    let invite = Invite::parse(&arg)?;
    println!("✓ parsed invite id: {}", invite.id);

    let token = invite.token()?;
    println!("✓ derived broker token ({} chars): {}…", token.len(), &token[..12.min(token.len())]);

    let http = reqwest::Client::new();
    let resp = http
        .get(format!("{BROKER_BASE}/metadata"))
        .query(&[("token", &token)])
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    println!("\nGET /metadata -> HTTP {status}");
    println!("{}", &body[..body.len().min(1500)]);

    if status.is_success() {
        println!("\nRESULT: ✅ the live broker ACCEPTED our token — stages ①–② are correct against reality.");
    } else {
        println!("\nRESULT: ❌ HTTP {status} — token rejected or endpoint changed; needs a look.");
    }
    Ok(())
}
