//! Minimal STUN (RFC 5389) wire handling for Teleport ICE — enough to gather a reflexive candidate and to
//! drive the nomination Binding exchange. Ported from the reference client's `stun.go` (which used Pion);
//! we hand-roll the bytes we need so there's no heavy dependency. MESSAGE-INTEGRITY (HMAC-SHA1, keyed by the
//! session secret) for the nomination phase is added in a later stage; this file covers the plain
//! Binding request + XOR-MAPPED-ADDRESS parsing, which are pure and unit-tested.

use std::net::SocketAddr;

/// The fixed STUN magic cookie (RFC 5389).
pub const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Is this datagram a STUN message? (length + first-two-bits-zero + magic cookie, per `isSTUN`.)
pub fn is_stun(data: &[u8]) -> bool {
    data.len() >= 20
        && (data[0] == 0x00 || data[0] == 0x01)
        && u32::from_be_bytes([data[4], data[5], data[6], data[7]]) == MAGIC_COOKIE
}

/// Build a plain STUN Binding request (20-byte header, no attributes) with the given transaction id.
pub fn binding_request(transaction_id: &[u8; 12]) -> Vec<u8> {
    let mut m = Vec::with_capacity(20);
    m.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes()); // message length (no attributes)
    m.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    m.extend_from_slice(transaction_id);
    m
}

/// Parse the XOR-MAPPED-ADDRESS (our reflexive address) from a STUN Binding response. IPv4 only — the
/// reflexive candidate we care about is IPv4. Returns `None` if absent or malformed.
pub fn parse_xor_mapped_address(data: &[u8]) -> Option<SocketAddr> {
    if !is_stun(data) {
        return None;
    }
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = (20 + msg_len).min(data.len());
    let mut i = 20;
    while i + 4 <= end {
        let atype = u16::from_be_bytes([data[i], data[i + 1]]);
        let alen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let v = i + 4;
        if v + alen > data.len() {
            break;
        }
        if atype == ATTR_XOR_MAPPED_ADDRESS && alen >= 8 && data[v + 1] == 0x01 {
            let port = u16::from_be_bytes([data[v + 2], data[v + 3]]) ^ ((MAGIC_COOKIE >> 16) as u16);
            let cookie = MAGIC_COOKIE.to_be_bytes();
            let ip = [
                data[v + 4] ^ cookie[0],
                data[v + 5] ^ cookie[1],
                data[v + 6] ^ cookie[2],
                data[v + 7] ^ cookie[3],
            ];
            return Some(SocketAddr::from((ip, port)));
        }
        i = v + ((alen + 3) & !3); // attributes are 4-byte aligned
    }
    None
}

const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const ATTR_DATA: u16 = 0x0013;
const BINDING_SUCCESS: u16 = 0x0101;

/// The 12-byte transaction id of a STUN message.
pub fn transaction_id(data: &[u8]) -> Option<[u8; 12]> {
    if !is_stun(data) {
        return None;
    }
    data[8..20].try_into().ok()
}

/// Find an attribute's value slice by type.
fn find_attribute(data: &[u8], want: u16) -> Option<(usize, &[u8])> {
    if !is_stun(data) {
        return None;
    }
    let end = (20 + u16::from_be_bytes([data[2], data[3]]) as usize).min(data.len());
    let mut i = 20;
    while i + 4 <= end {
        let atype = u16::from_be_bytes([data[i], data[i + 1]]);
        let alen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let v = i + 4;
        if v + alen > data.len() {
            break;
        }
        if atype == want {
            return Some((i, &data[v..v + alen]));
        }
        i = v + ((alen + 3) & !3);
    }
    None
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha1::Sha1>>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Append a MESSAGE-INTEGRITY attribute (RFC 5389 §15.4): HMAC-SHA1 over the message with the length field
/// pre-set to include the 24-byte integrity attribute. Short-term key = the secret itself.
pub fn append_message_integrity(msg: &mut Vec<u8>, key: &[u8]) {
    let len_including_mi = ((msg.len() - 20) + 24) as u16;
    msg[2..4].copy_from_slice(&len_including_mi.to_be_bytes());
    let mac = hmac_sha1(key, msg);
    msg.extend_from_slice(&ATTR_MESSAGE_INTEGRITY.to_be_bytes());
    msg.extend_from_slice(&20u16.to_be_bytes());
    msg.extend_from_slice(&mac);
}

/// Validate a message's MESSAGE-INTEGRITY against `key` (constant-time-ish compare of the 20-byte HMAC).
pub fn validate_message_integrity(data: &[u8], key: &[u8]) -> bool {
    let Some((mi_off, value)) = find_attribute(data, ATTR_MESSAGE_INTEGRITY) else {
        return false;
    };
    if value.len() != 20 {
        return false;
    }
    // Recompute over the message up to the integrity attribute, with the length field it was signed with.
    let mut prefix = data[..mi_off].to_vec();
    let len = ((mi_off - 20) + 24) as u16;
    prefix[2..4].copy_from_slice(&len.to_be_bytes());
    hmac_sha1(key, &prefix).as_slice() == value
}

/// Parse the nomination DATA payload (`{"wait": N}`) if present — the master's countdown value.
pub fn parse_nomination_wait(data: &[u8]) -> Option<i64> {
    let (_, v) = find_attribute(data, ATTR_DATA)?;
    serde_json::from_slice::<serde_json::Value>(v).ok()?.get("wait")?.as_i64()
}

/// Build a STUN Binding Success for `transaction_id` with MESSAGE-INTEGRITY — our reply to the console's
/// nomination Binding requests (we never add a DATA attribute; the console is the master).
pub fn binding_success(transaction_id: &[u8; 12], integrity_key: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(44);
    m.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    m.extend_from_slice(transaction_id);
    append_message_integrity(&mut m, integrity_key);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_recognizes_a_binding_request() {
        let req = binding_request(&[7u8; 12]);
        assert_eq!(req.len(), 20);
        assert_eq!(&req[0..2], &[0x00, 0x01]); // Binding Request
        assert_eq!(u32::from_be_bytes([req[4], req[5], req[6], req[7]]), MAGIC_COOKIE);
        assert!(is_stun(&req));
        assert!(!is_stun(&[0, 1, 2, 3]));
        assert!(!is_stun(&[0u8; 20])); // wrong cookie
    }

    #[test]
    fn parses_xor_mapped_address() {
        // Hand-build a Binding Success carrying XOR-MAPPED-ADDRESS for 128.76.158.114:54313.
        let (octets, port) = ([128u8, 76, 158, 114], 54313u16);
        let cookie = MAGIC_COOKIE.to_be_bytes();
        let xport = port ^ ((MAGIC_COOKIE >> 16) as u16);
        let xip = [
            octets[0] ^ cookie[0],
            octets[1] ^ cookie[1],
            octets[2] ^ cookie[2],
            octets[3] ^ cookie[3],
        ];
        let mut m = Vec::new();
        m.extend_from_slice(&0x0101u16.to_be_bytes()); // Binding Success
        m.extend_from_slice(&12u16.to_be_bytes()); // attribute section length
        m.extend_from_slice(&cookie);
        m.extend_from_slice(&[9u8; 12]); // transaction id
        m.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        m.extend_from_slice(&8u16.to_be_bytes());
        m.extend_from_slice(&[0x00, 0x01]); // reserved, family = IPv4
        m.extend_from_slice(&xport.to_be_bytes());
        m.extend_from_slice(&xip);

        assert_eq!(parse_xor_mapped_address(&m), Some(SocketAddr::from((octets, port))));
        assert_eq!(parse_xor_mapped_address(&binding_request(&[0u8; 12])), None); // no attribute
    }

    #[test]
    fn message_integrity_round_trips_and_carries_txid() {
        let key = b"session-secret";
        let msg = binding_success(&[3u8; 12], key);
        assert_eq!(&msg[0..2], &[0x01, 0x01]); // Binding Success
        assert!(validate_message_integrity(&msg, key));
        assert!(!validate_message_integrity(&msg, b"wrong-key"));
        assert_eq!(transaction_id(&msg), Some([3u8; 12]));
    }

    #[test]
    fn parses_the_nomination_wait_data() {
        let payload = br#"{"wait":500}"#;
        let mut m = binding_request(&[1u8; 12]);
        m.extend_from_slice(&ATTR_DATA.to_be_bytes());
        m.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        m.extend_from_slice(payload);
        while (m.len() - 20) % 4 != 0 {
            m.push(0);
        }
        let attrs_len = (m.len() - 20) as u16;
        m[2..4].copy_from_slice(&attrs_len.to_be_bytes());
        assert_eq!(parse_nomination_wait(&m), Some(500));
        assert_eq!(parse_nomination_wait(&binding_request(&[0u8; 12])), None);
    }
}
