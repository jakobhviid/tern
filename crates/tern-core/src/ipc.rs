//! Shared identifiers for the session-bus D-Bus service that `ternd` exposes and `tern`/`tern-gui` consume.
//! The wire contract is intentionally simple: methods take/return **JSON strings** (a [`crate::state::Snapshot`]
//! or an `{ok, error}` envelope), and a `Changed` signal carries the new snapshot JSON so clients update live.

/// Well-known bus name the daemon owns on the session bus.
pub const BUS_NAME: &str = "phd.hviid.Tern";
/// Object path the service is served at.
pub const OBJECT_PATH: &str = "/phd/hviid/Tern";
/// Interface name.
pub const INTERFACE: &str = "phd.hviid.Tern";
