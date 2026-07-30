//! The top-bar tray icon (StatusNotifierItem via `ksni`). It reflects the latest [`Snapshot`] and offers a
//! quick menu; menu actions are routed through the same channels the window uses. Runs on ksni's own thread
//! (the blocking API), so there's no tokio/glib runtime mixing. Verified at runtime on Linux (needs an SNI
//! host — e.g. KDE, or GNOME with the AppIndicator extension).

use ksni::menu::StandardItem;
use ksni::{MenuItem, ToolTip, Tray};
use tern_core::ipc::APP_ID;
use tern_core::state::{Access, Snapshot, TrayVisual};

use crate::{Cmd, Update};

pub struct TernTray {
    pub snapshot: Snapshot,
    /// Commands to the D-Bus actor (Access on/off, sign out).
    pub cmd_tx: async_channel::Sender<Cmd>,
    /// Window-control messages to the GTK loop (present, quit).
    pub gui_tx: async_channel::Sender<Update>,
}

impl Tray for TernTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "Tern".into()
    }

    fn icon_name(&self) -> String {
        // Themed symbolic icons present on typical GNOME/KDE systems, varying by state.
        match self.snapshot.tray_visual() {
            TrayVisual::Active => "network-vpn-symbolic".into(),
            TrayVisual::Working => "network-vpn-acquiring-symbolic".into(),
            TrayVisual::Warning => "network-vpn-no-route-symbolic".into(),
            TrayVisual::Neutral => "network-vpn-disconnected-symbolic".into(),
        }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
            title: "Tern".into(),
            description: self.snapshot.summary_line(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let access_on = matches!(self.snapshot.access, Access::On | Access::TurningOn);
        let toggle_label = if access_on { "Disconnect" } else { "Connect" };
        vec![
            StandardItem {
                label: "Open Tern".into(),
                activate: Box::new(|t: &mut TernTray| {
                    let _ = t.gui_tx.try_send(Update::Present);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: toggle_label.into(),
                activate: Box::new(|t: &mut TernTray| {
                    let on = matches!(t.snapshot.access, Access::On | Access::TurningOn);
                    let cmd = if on { Cmd::Disconnect } else { Cmd::Connect(String::new()) };
                    let _ = t.cmd_tx.try_send(cmd);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Forget this console".into(),
                activate: Box::new(|t: &mut TernTray| {
                    let _ = t.cmd_tx.try_send(Cmd::SignOut);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut TernTray| {
                    // Disconnect the tunnel, then quit (handled by the actor) — no orphan tunnel on exit.
                    let _ = t.cmd_tx.try_send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
