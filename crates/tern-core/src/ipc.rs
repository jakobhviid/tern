//! Shared identifiers for the session-bus D-Bus service that `ternd` exposes and `tern`/`tern-gui` consume.
//! The wire contract is intentionally simple: methods take/return **JSON strings** (a [`crate::state::Snapshot`]
//! or an `{ok, error}` envelope), and a `Changed` signal carries the new snapshot JSON so clients update live.

use serde::{Deserialize, Serialize};

use crate::error::UserFacing;

/// Well-known bus name the daemon owns on the session bus.
pub const BUS_NAME: &str = "phd.hviid.Tern";
/// Object path the service is served at.
pub const OBJECT_PATH: &str = "/phd/hviid/Tern";
/// Interface name.
pub const INTERFACE: &str = "phd.hviid.Tern";

/// Result envelope returned by the daemon's action methods. On failure, `error` is the plain-language
/// rendering (title + recovery action), so a client can show it verbatim without knowing anything technical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<UserFacing>,
}

impl ActionResult {
    pub fn ok() -> Self {
        Self { ok: true, error: None }
    }
    pub fn failed(error: UserFacing) -> Self {
        Self { ok: false, error: Some(error) }
    }
}
