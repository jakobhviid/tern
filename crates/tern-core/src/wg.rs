//! WireGuard key material and config rendering.
//!
//! Keys are X25519 via `x25519-dalek` (BSD/MIT, no OpenSSL). The client generates its own keypair and
//! uploads the **public** key during device enrollment (docs/02); the private key never leaves the device
//! and is stored in the system keyring.

use base64::Engine as _;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A WireGuard keypair in `wg`-compatible base64 form.
#[derive(Clone)]
pub struct KeyPair {
    /// Base64 of the 32-byte private key. Secret — persist only in the keyring.
    pub private: String,
    /// Base64 of the 32-byte public key. Safe to send to the server.
    pub public: String,
}

/// Generate a fresh WireGuard keypair using the OS CSPRNG.
pub fn generate_keypair() -> KeyPair {
    let secret = StaticSecret::random_from_rng(rand_core::OsRng);
    let public = PublicKey::from(&secret);
    let mut secret_bytes = secret.to_bytes();
    let kp = KeyPair {
        private: B64.encode(secret_bytes),
        public: B64.encode(public.to_bytes()),
    };
    secret_bytes.zeroize();
    kp
}

/// Derive the base64 public key from a base64 private key (used to reconstruct the public key from a stored
/// private key without persisting both). Returns `None` if the input isn't a valid 32-byte key.
pub fn public_from_private(private_b64: &str) -> Option<String> {
    let bytes = B64.decode(private_b64).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    let secret = StaticSecret::from(arr);
    Some(B64.encode(PublicKey::from(&secret).to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_is_valid_wireguard_material() {
        let kp = generate_keypair();
        assert_eq!(B64.decode(&kp.private).unwrap().len(), 32);
        assert_eq!(B64.decode(&kp.public).unwrap().len(), 32);
        assert_ne!(kp.private, kp.public);
    }

    #[test]
    fn public_is_deterministic_from_private() {
        let kp = generate_keypair();
        assert_eq!(public_from_private(&kp.private).as_deref(), Some(kp.public.as_str()));
    }

    #[test]
    fn rejects_garbage_private_key() {
        assert_eq!(public_from_private("not-base64!!"), None);
        assert_eq!(public_from_private("dG9vc2hvcnQ="), None); // "tooshort" -> 8 bytes
    }
}
