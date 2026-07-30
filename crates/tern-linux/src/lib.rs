//! Linux system-integration backends for `tern`, implemented by driving the standard desktop CLIs
//! (`nmcli`, `gio`, `secret-tool`) as **arm's-length subprocesses**. Rationale (see DECISIONS):
//! - **Unprivileged**: NetworkManager holds `CAP_NET_ADMIN`; we only ask it to activate a user-owned
//!   connection (ADR-0004).
//! - **License-clean**: GVfs keeps GPL-3 `libsmbclient` out of our process (ADR-0005/0007); exec is mere
//!   aggregation, not linking.
//! - **Verifiable anywhere**: no Linux-only crates, so the whole workspace still builds on macOS/CI. Native
//!   libraries (libnm, oo7) are a future refinement; runtime behavior is validated on Linux (Bazzite).

use std::sync::Arc;

use tern_core::backend::{MountBackend, Reachability, SecretStore, VpnBackend};

pub mod cmd;
pub mod gvfs;
pub mod nm;
pub mod reach;
pub mod secret;

/// The four backend seams the engine needs (VPN, mounts, reachability, secrets).
pub type Backends = (
    Arc<dyn VpnBackend>,
    Arc<dyn MountBackend>,
    Arc<dyn Reachability>,
    Arc<dyn SecretStore>,
);

/// Build the Linux backend set for the daemon.
pub fn backends() -> Backends {
    (
        Arc::new(nm::NmVpn::new()),
        Arc::new(gvfs::GvfsMounts::new()),
        Arc::new(reach::TcpReachability::new()),
        Arc::new(secret::KeyringSecrets::new()),
    )
}
