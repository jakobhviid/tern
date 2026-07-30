//! Reachability probe. A short TCP connect to the console's WAN endpoint tells us whether a network path
//! exists (on the LAN or over the tunnel). This is deliberately simple for now.
//! TODO(bazzite): distinguish LAN vs VPN, and probe the drive's SMB host:445 rather than the console.

use std::time::Duration;

use async_trait::async_trait;
use tern_core::backend::{Reach, Reachability};
use tern_core::model::Host;

pub struct TcpReachability;

impl TcpReachability {
    pub fn new() -> Self {
        TcpReachability
    }
}

impl Default for TcpReachability {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Reachability for TcpReachability {
    async fn reach(&self, host: &Host) -> Reach {
        let Some(ip) = host.wan_ip.as_deref() else {
            return Reach::Unreachable;
        };
        let addr = format!("{ip}:443");
        let reachable = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false);
        if reachable {
            Reach::Vpn
        } else {
            Reach::Unreachable
        }
    }
}
