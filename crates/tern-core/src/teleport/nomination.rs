//! Teleport nomination (ADR-0016 stage 4). After CONNECT, the console (the **master**) sends authenticated
//! STUN Binding requests to our candidates, each carrying a DATA `wait` value in a fixed countdown; we (the
//! **slave**) validate MESSAGE-INTEGRITY, reply with a Binding Success, and record the wait sequence per
//! remote tuple. The tuple that delivers the whole sequence in order is the **nominated endpoint** the
//! WireGuard tunnel then uses. We must never originate DATA — doing so reverses the role and the console
//! won't activate WireGuard. Ports the reference `nomination.go`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;

use super::stun;

/// The master's countdown; a remote tuple that delivers all of these in order (`nominationWaitSequence`) is
/// the nominated endpoint.
const WAIT_SEQUENCE: [i64; 5] = [2000, 1000, 500, 250, 125];

/// Listen on `socket` for the console's authenticated nomination Binding requests (keyed by `stun_secret`),
/// reply to each, and return the first remote tuple that completes the wait sequence — the nominated
/// endpoint. Returns `None` on timeout. Non-STUN datagrams are ignored (the data plane consumes those).
pub async fn await_nomination(
    socket: &UdpSocket,
    stun_secret: &str,
    timeout: Duration,
) -> Option<SocketAddr> {
    let key = stun_secret.as_bytes();
    let mut progress: HashMap<SocketAddr, usize> = HashMap::new();
    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let (n, from) = match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            _ => return None, // timed out or socket error
        };
        let data = &buf[..n];
        // Only authenticated Binding requests are trusted; everything else is ignored here.
        if !stun::is_stun(data) || data[0..2] != [0x00, 0x01] || !stun::validate_message_integrity(data, key) {
            continue;
        }
        if let Some(txid) = stun::transaction_id(data) {
            let _ = socket.send_to(&stun::binding_success(&txid, key), from).await;
        }
        if let Some(wait) = stun::parse_nomination_wait(data) {
            let next = progress.entry(from).or_insert(0);
            if *next < WAIT_SEQUENCE.len() && wait == WAIT_SEQUENCE[*next] {
                *next += 1;
                if *next == WAIT_SEQUENCE.len() {
                    return Some(from);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nominates_the_tuple_that_completes_the_sequence() {
        // A local "console" socket drives the full wait sequence at our listener; expect it to be nominated.
        let secret = "session-secret";
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();
        let console = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let console_addr = console.local_addr().unwrap();

        let driver = tokio::spawn(async move {
            for wait in WAIT_SEQUENCE {
                // Binding request (type 0x0001) with a DATA {"wait":N} attribute + MESSAGE-INTEGRITY.
                let mut m = stun::binding_request(&[9u8; 12]);
                let payload = format!("{{\"wait\":{wait}}}");
                m.extend_from_slice(&0x0013u16.to_be_bytes());
                m.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                m.extend_from_slice(payload.as_bytes());
                while (m.len() - 20) % 4 != 0 {
                    m.push(0);
                }
                let attrs = (m.len() - 20) as u16;
                m[2..4].copy_from_slice(&attrs.to_be_bytes());
                stun::append_message_integrity(&mut m, secret.as_bytes());
                console.send_to(&m, listener_addr).await.unwrap();
                let mut b = [0u8; 128];
                let _ = tokio::time::timeout(Duration::from_millis(200), console.recv_from(&mut b)).await;
            }
        });

        let nominated = await_nomination(&listener, secret, Duration::from_secs(2)).await;
        driver.await.unwrap();
        assert_eq!(nominated, Some(console_addr));
    }
}
