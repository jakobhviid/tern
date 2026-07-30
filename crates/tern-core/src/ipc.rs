//! Shared identifiers for the session-bus D-Bus service that `ternd` exposes and `tern`/`tern-gui` consume.
//! The wire contract is intentionally simple: methods take/return **JSON strings** (a [`crate::state::Snapshot`]
//! or an `{ok, error}` envelope), and a `Changed` signal carries the new snapshot JSON so clients update live.

use serde::{Deserialize, Serialize};

use crate::error::UserFacing;

/// Well-known bus name the daemon owns on the session bus. This is a **sub-name** of the desktop
/// [`APP_ID`] so it never collides with the GUI's `GtkApplication`, which owns the bare `APP_ID` on the
/// same bus (GNOME/Flatpak convention: the user-facing app owns the app-id; helpers take sub-names).
pub const BUS_NAME: &str = "phd.hviid.Tern.Daemon";
/// Object path the service is served at.
pub const OBJECT_PATH: &str = "/phd/hviid/Tern";
/// Interface name.
pub const INTERFACE: &str = "phd.hviid.Tern.Daemon";
/// Desktop application id: the `.desktop`/icon/metainfo/Flatpak identity **and** the GUI's
/// `GtkApplication` id (hence the Wayland `app_id`/WM class). Distinct from [`BUS_NAME`] so the GUI can
/// own it on the session bus without fighting the daemon for the same name.
pub const APP_ID: &str = "phd.hviid.Tern";

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
