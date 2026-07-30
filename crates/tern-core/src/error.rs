//! Error taxonomy and its mapping to calm, plain-language user messages.
//!
//! Rule (docs/05): the user sees `UserFacing.title` (+ optional short `detail`) and one recovery
//! [`UserAction`]. Raw technical text (HTTP codes, D-Bus errors, host IDs, stack traces) never goes in the
//! title — it belongs only in the "Copy details for support" affordance, carried separately.

use serde::{Deserialize, Serialize};

/// The single recovery action offered for a problem — drives the primary button in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserAction {
    /// Nothing to do (informational / auto-recovering).
    None,
    /// Try the same thing again.
    Retry,
    /// (Re)start sign-in.
    SignIn,
    /// Enter a multi-factor code.
    EnterCode,
    /// Approve a push prompt on the phone (waiting).
    ApproveOnPhone,
    /// The org/admin must act — user can't self-resolve.
    ContactAdmin,
    /// Turn on Access (the VPN) to reach the resource.
    TurnOnAccess,
    /// Reconnect Access.
    Reconnect,
    /// Enter file-service credentials for a drive.
    EnterCredentials,
    /// Unlock the system keyring.
    UnlockKeyring,
    /// Open a help/support page.
    HelpLink,
}

impl UserAction {
    /// Short label for the primary button.
    pub fn label(self) -> &'static str {
        match self {
            UserAction::None => "OK",
            UserAction::Retry => "Retry",
            UserAction::SignIn => "Sign in",
            UserAction::EnterCode => "Enter code",
            UserAction::ApproveOnPhone => "Waiting…",
            UserAction::ContactAdmin => "Contact admin",
            UserAction::TurnOnAccess => "Turn on Access",
            UserAction::Reconnect => "Reconnect",
            UserAction::EnterCredentials => "Enter credentials",
            UserAction::UnlockKeyring => "Unlock",
            UserAction::HelpLink => "Help",
        }
    }
}

/// A user-facing rendering of a problem: one calm line, optional short detail, and a recovery action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserFacing {
    pub title: String,
    pub detail: Option<String>,
    pub action: UserAction,
}

impl UserFacing {
    fn new(title: &str, action: UserAction) -> Self {
        Self { title: title.to_string(), detail: None, action }
    }
    fn with_detail(title: &str, detail: String, action: UserAction) -> Self {
        Self { title: title.to_string(), detail: Some(detail), action }
    }
}

/// All the ways things fail, grouped by cause so the UI can map each to one recovery.
///
/// Keep the `#[error(...)]` strings *technical* (they feed logs and the support-details view); the
/// human-facing wording is produced by [`Error::user_facing`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ---- authentication ----
    #[error("organization not found")]
    OrgNotFound,
    #[error("multi-factor code required")]
    MfaRequired,
    #[error("waiting for push approval")]
    MfaPushPending,
    #[error("session expired")]
    SessionExpired,
    /// Server-provided plain reason (locked/disabled/expired/revoked) — safe to echo to the user.
    #[error("account restricted: {0}")]
    AccountRestricted(String),
    #[error("this workspace is blocked by policy")]
    WorkspaceBlocked,
    /// The pasted Teleport invite link/code isn't a valid invite.
    #[error("invalid teleport invite: {0}")]
    InvalidInvite(String),

    // ---- VPN / access ----
    #[error("no console available for this account")]
    NoConsoleAvailable,
    #[error("could not reach the network (handshake timed out)")]
    VpnUnreachable,
    #[error("tunnel is up but not passing traffic")]
    VpnDegraded,
    #[error("NetworkManager is not available on this host")]
    NetworkManagerMissing,
    #[error("permission to change the connection was denied")]
    PolkitDenied,
    /// The console is only reachable via UniFi's proprietary relay (out of scope) — be honest, not fake-success.
    #[error("console only reachable via vendor relay (unsupported)")]
    RelayOnly,

    // ---- drives ----
    #[error("drive unreachable")]
    DriveUnreachable,
    #[error("file-service credentials rejected")]
    DriveCredentialsRejected,
    #[error("encrypted drive is locked")]
    DriveLocked,
    #[error("failed to mount drive: {0}")]
    DriveMountFailed(String),

    // ---- system ----
    #[error("keyring is locked or unavailable")]
    KeyringLocked,
    #[error("no internet connection")]
    Offline,

    // ---- transport / unexpected ----
    #[error("network request failed: {0}")]
    Http(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Map to a plain-language, actionable message. `detail` (when present) is the technical text for the
    /// "Copy details for support" affordance — never surface it as the title.
    pub fn user_facing(&self) -> UserFacing {
        use Error::*;
        use UserAction as A;
        match self {
            OrgNotFound => UserFacing::new(
                "We couldn't find that organization. Check the address from your invitation.",
                A::SignIn,
            ),
            MfaRequired => UserFacing::new(
                "Enter the verification code from your authenticator.",
                A::EnterCode,
            ),
            MfaPushPending => UserFacing::new(
                "Approve the sign-in request on your phone.",
                A::ApproveOnPhone,
            ),
            SessionExpired => UserFacing::new(
                "Your session expired. Sign in again to stay connected.",
                A::SignIn,
            ),
            AccountRestricted(reason) => UserFacing::with_detail(
                "There's a problem with your account. Contact your admin.",
                reason.clone(),
                A::ContactAdmin,
            ),
            WorkspaceBlocked => UserFacing::new(
                "This workspace is blocked by your organization's policy. Contact your admin.",
                A::ContactAdmin,
            ),
            InvalidInvite(_) => UserFacing::new(
                "That invite link doesn't look right. Copy it again from your console.",
                A::Retry,
            ),
            NoConsoleAvailable => UserFacing::new(
                "Your network isn't available right now. Try again in a moment.",
                A::Retry,
            ),
            VpnUnreachable => UserFacing::new(
                "Couldn't connect to your network. It may be offline or unreachable.",
                A::Retry,
            ),
            VpnDegraded => UserFacing::new(
                "Access is on but not working. Reconnecting may help.",
                A::Reconnect,
            ),
            NetworkManagerMissing => UserFacing::new(
                "This system needs NetworkManager to manage the connection.",
                A::HelpLink,
            ),
            PolkitDenied => UserFacing::new(
                "Permission was needed to change the connection and wasn't granted.",
                A::Retry,
            ),
            RelayOnly => UserFacing::new(
                "This network can't be reached from here yet.",
                A::HelpLink,
            ),
            DriveUnreachable => UserFacing::new(
                "This drive is unavailable. Turn on Access to reach it.",
                A::TurnOnAccess,
            ),
            DriveCredentialsRejected => UserFacing::new(
                "Sign-in needed for this drive. Enter your file access credentials.",
                A::EnterCredentials,
            ),
            DriveLocked => UserFacing::new(
                "This drive is locked. Ask your admin to unlock it.",
                A::ContactAdmin,
            ),
            DriveMountFailed(detail) => UserFacing::with_detail(
                "Couldn't mount this drive. Try again in a moment.",
                detail.clone(),
                A::Retry,
            ),
            KeyringLocked => UserFacing::new(
                "Unlock your keyring to save your credentials.",
                A::UnlockKeyring,
            ),
            Offline => UserFacing::new(
                "You're offline. We'll reconnect when you're back.",
                A::None,
            ),
            Http(detail) => UserFacing::with_detail(
                "Something went wrong reaching the service. Please try again.",
                detail.clone(),
                A::Retry,
            ),
            Other(e) => UserFacing::with_detail(
                "Something went wrong. Please try again.",
                e.to_string(),
                A::Retry,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_nonempty_title_and_no_jargon_in_title() {
        // A representative spread across every group.
        let samples = [
            Error::OrgNotFound,
            Error::MfaRequired,
            Error::SessionExpired,
            Error::AccountRestricted("account is locked".into()),
            Error::NoConsoleAvailable,
            Error::VpnUnreachable,
            Error::VpnDegraded,
            Error::NetworkManagerMissing,
            Error::DriveUnreachable,
            Error::DriveCredentialsRejected,
            Error::DriveLocked,
            Error::KeyringLocked,
            Error::Offline,
            Error::Http("500 Internal Server Error".into()),
            Error::InvalidInvite("https://teleport.ui.link/not-a-uuid".into()),
        ];
        // Whole words we must never leak into a user-facing title (docs/05 anti-patterns). Matched
        // per-token so a term like "ice" doesn't false-positive inside "service".
        let jargon = [
            "wireguard", "smb", "cifs", "dbus", "sigv4", "ice", "stun", "netstack", "ucs", "polkit",
            "http", "json", "consolestandardid", "handshake", "tunnel", "wg", "vpn",
        ];
        for e in samples {
            let uf = e.user_facing();
            assert!(!uf.title.is_empty(), "empty title for {e:?}");
            let words: std::collections::HashSet<&str> = uf
                .title
                .split(|c: char| !c.is_ascii_alphanumeric())
                .map(|w| w.trim())
                .filter(|w| !w.is_empty())
                .collect();
            let lower: std::collections::HashSet<String> =
                words.iter().map(|w| w.to_lowercase()).collect();
            for j in jargon {
                assert!(!lower.contains(j), "jargon {j:?} leaked into title {:?}", uf.title);
            }
        }
    }

    #[test]
    fn server_reason_is_carried_in_detail_not_title() {
        let uf = Error::AccountRestricted("your account has been locked".into()).user_facing();
        assert_eq!(uf.action, UserAction::ContactAdmin);
        assert_eq!(uf.detail.as_deref(), Some("your account has been locked"));
    }
}
