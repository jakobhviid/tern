//! `tern-core` — platform-agnostic core for an unofficial UniFi Identity ("UniFi Endpoint") client.
//!
//! Design rules (see `ARCHITECTURE.md` and `docs/`):
//! - **No GUI, no GTK, no Linux-only syscalls here.** Everything in this crate builds and is tested on any
//!   platform (including macOS/CI), so the auth / UCS / state logic can be exercised without a display,
//!   D-Bus, or a real UniFi account.
//! - System integration (NetworkManager, GVfs, keyring) lives behind traits (added in `backend`) and is
//!   implemented in the `tern-linux` crate.
//! - User-facing wording lives in [`error`] (see `docs/05-ux-and-error-handling-guidelines.md`): never
//!   surface protocol/implementation detail to end users.

pub mod auth;
pub mod backend;
pub mod config;
pub mod engine;
pub mod error;
pub mod ipc;
pub mod model;
pub mod state;
pub mod teleport;
pub mod ucs;
pub mod wg;

pub use error::{Error, UserAction, UserFacing};

/// Crate result type: fallible operations return a domain [`Error`] that already knows how to render
/// itself for a non-technical user via [`Error::user_facing`].
pub type Result<T> = std::result::Result<T, Error>;
