//! The app's user-visible state machine (see `docs/05-ux-and-error-handling-guidelines.md` §4).
//!
//! Internal machinery (WireGuard, UCS sessions, SMB) is deliberately *not* modelled here — this is the
//! small set of states a person experiences, each mapping to one label + one recovery action.

use serde::{Deserialize, Serialize};

use crate::model::{Drive, Identity};

/// Overall sign-in state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Auth {
    SignedOut,
    SigningIn,
    SignedIn(Identity),
    SessionExpired,
}

/// The "Access" (VPN) state as the user experiences it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Off,
    TurningOn,
    On,
    /// Tunnel is up but not passing traffic/DNS.
    Degraded,
    /// Couldn't reach the network at all.
    Unreachable,
}

/// Per-drive state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveMount {
    /// Selected but nothing attempted yet.
    Idle,
    /// Reachable (LAN or via Access) but not yet mounted.
    Reachable,
    Mounting,
    Mounted,
    /// Selected but not reachable right now (needs LAN or Access).
    Unavailable,
    CredentialsNeeded,
    Locked,
    Failed,
}

impl DriveMount {
    /// Human label for a drive row (docs/05 §4).
    pub fn label(self) -> &'static str {
        match self {
            DriveMount::Idle => "Not mounted",
            DriveMount::Reachable => "Ready to mount",
            DriveMount::Mounting => "Mounting…",
            DriveMount::Mounted => "Mounted",
            DriveMount::Unavailable => "Unavailable — turn on Access",
            DriveMount::CredentialsNeeded => "Sign-in needed",
            DriveMount::Locked => "Locked",
            DriveMount::Failed => "Couldn't mount",
        }
    }
}

/// A drive plus its current mount state and whether it's selected for auto-mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveStatus {
    pub drive: Drive,
    pub state: DriveMount,
    /// Whether the user has ticked this drive to auto-mount.
    pub selected: bool,
}

/// The overall tray/menu-bar visual state (docs/05 §5) — kept to a handful a person can read at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayVisual {
    /// Off / neutral (signed out, or Access off).
    Neutral,
    /// Working (signing in / turning on).
    Working,
    /// On and healthy.
    Active,
    /// Attention needed (degraded / unreachable / session expired).
    Warning,
}

/// A consistent snapshot of everything the UI renders. Produced by the daemon; consumed by tray/GUI/CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub auth: Auth,
    pub access: Access,
    pub drives: Vec<DriveStatus>,
}

impl Snapshot {
    pub fn signed_out() -> Self {
        Snapshot { auth: Auth::SignedOut, access: Access::Off, drives: Vec::new() }
    }

    /// The one-line status shown in the tray header / tooltip.
    pub fn summary_line(&self) -> String {
        match &self.auth {
            Auth::SigningIn => return "Signing you in…".to_string(),
            Auth::SessionExpired => return "Session expired".to_string(),
            // Signed out with nothing running = genuinely idle. But a Teleport invite or an imported config
            // brings a tunnel up *without* an account (ADR-0016/0004), so fall through to the Access line
            // when Access isn't Off — otherwise "Not signed in" would show while connected.
            Auth::SignedOut if self.access == Access::Off => return "Not signed in".to_string(),
            Auth::SignedOut | Auth::SignedIn(_) => {}
        }
        let access = match self.access {
            Access::Off => "Access off",
            Access::TurningOn => "Turning on Access…",
            Access::On => "Access on",
            Access::Degraded => "Access on, but not working",
            Access::Unreachable => "Can't reach your network",
        };
        let mounted = self.mounted_count();
        if mounted > 0 {
            format!("{access} · {mounted} drive{} mounted", if mounted == 1 { "" } else { "s" })
        } else {
            access.to_string()
        }
    }

    /// How many selected drives are currently mounted.
    pub fn mounted_count(&self) -> usize {
        self.drives.iter().filter(|d| d.state == DriveMount::Mounted).count()
    }

    /// The tray icon visual (docs/05 §5).
    pub fn tray_visual(&self) -> TrayVisual {
        match &self.auth {
            Auth::SigningIn => return TrayVisual::Working,
            Auth::SessionExpired => return TrayVisual::Warning,
            // As with summary_line: a Teleport/imported tunnel can be up while signed out, so reflect Access
            // rather than always showing Neutral.
            Auth::SignedOut if self.access == Access::Off => return TrayVisual::Neutral,
            Auth::SignedOut | Auth::SignedIn(_) => {}
        }
        match self.access {
            Access::Off => TrayVisual::Neutral,
            Access::TurningOn => TrayVisual::Working,
            Access::On => TrayVisual::Active,
            Access::Degraded | Access::Unreachable => TrayVisual::Warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Identity {
        Identity { email: "jah@example.com".into(), display_name: None, organization: None }
    }
    fn drive(name: &str, state: DriveMount) -> DriveStatus {
        DriveStatus {
            drive: Drive { id: name.into(), name: name.into(), share: None, encrypted: false },
            state,
            selected: false,
        }
    }

    #[test]
    fn summary_reflects_signed_out() {
        assert_eq!(Snapshot::signed_out().summary_line(), "Not signed in");
        assert_eq!(Snapshot::signed_out().tray_visual(), TrayVisual::Neutral);
    }

    #[test]
    fn accountless_teleport_connection_reflects_access_not_signed_out() {
        // A Teleport invite (or imported config) brings a tunnel up without an account: auth stays SignedOut
        // but the status must show the tunnel, not "Not signed in".
        let s = Snapshot { auth: Auth::SignedOut, access: Access::On, drives: vec![] };
        assert_eq!(s.summary_line(), "Access on");
        assert_eq!(s.tray_visual(), TrayVisual::Active);

        let turning = Snapshot { auth: Auth::SignedOut, access: Access::TurningOn, drives: vec![] };
        assert_eq!(turning.summary_line(), "Turning on Access…");
        assert_eq!(turning.tray_visual(), TrayVisual::Working);
    }

    #[test]
    fn summary_counts_mounted_drives() {
        let s = Snapshot {
            auth: Auth::SignedIn(id()),
            access: Access::On,
            drives: vec![
                drive("Design", DriveMount::Mounted),
                drive("Shared", DriveMount::Mounted),
                drive("Archive", DriveMount::Unavailable),
            ],
        };
        assert_eq!(s.summary_line(), "Access on · 2 drives mounted");
        assert_eq!(s.tray_visual(), TrayVisual::Active);
    }

    #[test]
    fn singular_drive_wording() {
        let s = Snapshot {
            auth: Auth::SignedIn(id()),
            access: Access::On,
            drives: vec![drive("Design", DriveMount::Mounted)],
        };
        assert_eq!(s.summary_line(), "Access on · 1 drive mounted");
    }

    #[test]
    fn degraded_access_is_a_warning_without_fake_success() {
        let s = Snapshot { auth: Auth::SignedIn(id()), access: Access::Degraded, drives: vec![] };
        assert_eq!(s.summary_line(), "Access on, but not working");
        assert_eq!(s.tray_visual(), TrayVisual::Warning);
    }
}
