//! UniFi Teleport client (ADR-0016) — the path that actually connects for a consumer account.
//!
//! Onboarding is a **Teleport invite** the console generates (Settings → VPN → Teleport), of the form
//! `https://teleport.ui.link/<uuid>`. On desktop that link is just a Firebase Dynamic Link to the WiFiman
//! mobile app (no custom scheme to register), so the user **pastes it** (or its bare UUID) and we act on it —
//! `Invite::parse` below turns either into a validated invite id.
//!
//! This module is a clean-room Rust port of the reverse-engineered Teleport protocol (reference:
//! `sinnet3000/teleport-client`, MIT — validated end-to-end on 2026-07-30). Stage ① implemented here is the
//! invite; later stages add broker pairing (`cloudaccess.svc.ui.com/teleport`), ICE/STUN nomination, and the
//! userspace-WireGuard data plane. Permissive crates only (ADR-0007): `boringtun`/`str0m`/`smoltcp`.

use crate::{Error, Result};

/// Host a Teleport invite URL must use, so a stray link can't send us pairing somewhere else.
const INVITE_HOST: &str = "teleport.ui.link";

/// A parsed Teleport invite: a console-generated capability, **single-use** for the first pairing (afterwards
/// a saved session is reused — see the planned session store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The invite UUID, canonicalised to lowercase.
    pub id: String,
}

impl Invite {
    /// Parse what a user pastes from the browser: a full `https://teleport.ui.link/<uuid>` URL (query/fragment
    /// tolerated), or a bare UUID. Any other host, or a non-UUID, is rejected as [`Error::InvalidInvite`].
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim();
        let raw = if s.contains("://") {
            let url = url::Url::parse(s).map_err(|_| invalid(input))?;
            if !url.host_str().is_some_and(|h| h.eq_ignore_ascii_case(INVITE_HOST)) {
                return Err(invalid(input));
            }
            url.path_segments()
                .and_then(|mut segs| segs.next())
                .unwrap_or_default()
                .to_owned()
        } else {
            // Bare UUID (tolerate a stray `?…`/`#…` if someone pasted a partial URL without the scheme).
            s.split(['?', '#']).next().unwrap_or(s).trim().to_owned()
        };
        let id = raw.to_ascii_lowercase();
        if is_uuid(&id) {
            Ok(Invite { id })
        } else {
            Err(invalid(input))
        }
    }
}

fn invalid(input: &str) -> Error {
    Error::InvalidInvite(input.to_string())
}

/// True if `s` is a canonical 8-4-4-4-12 hex UUID (lowercase already applied by the caller).
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "08c9dc13-64bc-4525-9d9d-4659e6286f09";

    #[test]
    fn parses_a_full_invite_url() {
        assert_eq!(Invite::parse(&format!("https://teleport.ui.link/{UUID}")).unwrap().id, UUID);
    }

    #[test]
    fn parses_url_with_query_and_whitespace() {
        // Firebase appends `?l=1`; users paste with stray spaces/newlines.
        let pasted = format!("  https://teleport.ui.link/{UUID}?l=1\n");
        assert_eq!(Invite::parse(&pasted).unwrap().id, UUID);
    }

    #[test]
    fn parses_a_bare_uuid_and_lowercases() {
        assert_eq!(Invite::parse(UUID).unwrap().id, UUID);
        assert_eq!(Invite::parse(&UUID.to_uppercase()).unwrap().id, UUID);
    }

    #[test]
    fn rejects_wrong_host() {
        // A look-alike host must not be accepted — we'd pair against the wrong place.
        assert!(matches!(
            Invite::parse(&format!("https://evil.example.com/{UUID}")),
            Err(Error::InvalidInvite(_))
        ));
    }

    #[test]
    fn rejects_non_uuid() {
        for bad in ["", "not-an-invite", "https://teleport.ui.link/", "12345"] {
            assert!(matches!(Invite::parse(bad), Err(Error::InvalidInvite(_))), "should reject {bad:?}");
        }
    }
}
