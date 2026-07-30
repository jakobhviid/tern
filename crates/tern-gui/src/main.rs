//! `tern-gui` — the GNOME-native (GTK4 + libadwaita) front-end + top-bar tray. It is a thin client of
//! `ternd`: a background thread runs a tokio runtime that holds the D-Bus proxy, executes commands, and
//! streams live `Changed` updates; the GTK main thread renders the [`Snapshot`] and the tray reflects it.
//! The three threads (GTK, tokio actor, ksni tray) talk over `async-channel`, so no runtime mixing is needed.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use futures_util::StreamExt;
use gtk::glib;
use ksni::blocking::TrayMethods;
use tern_core::ipc::{ActionResult, APP_ID};
use tern_core::state::{Access, Auth, Snapshot};
use tern_core::teleport::Invite;

mod tray;
use tray::TernTray;

/// Commands from the UI/tray to the D-Bus actor.
enum Cmd {
    RedeemInvite(String),
    Connect(String),
    Disconnect,
    SignOut,
    SetAutoMount(String, bool),
    Refresh,
}

/// Updates from the actor/tray to the GTK main loop.
enum Update {
    Snapshot(Box<Snapshot>),
    Disconnected(String),
    /// An action failed — show its plain-language message as an in-window toast.
    Toast(String),
    /// Show the window (from the tray "Open").
    Present,
    /// Quit the app (from the tray "Quit").
    Quit,
}

#[zbus::proxy(
    interface = "phd.hviid.Tern.Daemon",
    default_service = "phd.hviid.Tern.Daemon",
    default_path = "/phd/hviid/Tern"
)]
trait Tern {
    async fn snapshot(&self) -> zbus::Result<String>;
    async fn redeem_invite(&self, url: &str) -> zbus::Result<String>;
    async fn connect(&self, console_id: &str) -> zbus::Result<String>;
    async fn disconnect(&self) -> zbus::Result<String>;
    async fn sign_out(&self) -> zbus::Result<String>;
    async fn set_auto_mount(&self, drive_id: &str, on: bool) -> zbus::Result<String>;
    #[zbus(signal)]
    async fn changed(&self, snapshot_json: String) -> zbus::Result<()>;
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "tern_gui=info".into()))
        .init();

    // Deep-link entry point. We're launched as `tern-gui %u` and registered for the
    // `x-scheme-handler/identity-standard` scheme (see the .desktop), so a UniFi Identity invite's
    // "open in app" hands us a URI carrying the enrollment payload. Capture it before GTK touches argv:
    // log it, and persist it to $XDG_RUNTIME_DIR/tern-deeplink.log so the sign-in flow (and bring-up
    // debugging) can see exactly what UniFi passes.
    for arg in std::env::args().skip(1) {
        if arg.contains("://") {
            tracing::info!(uri = %arg, "received deep-link invocation");
            let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{dir}/tern-deeplink.log"))
            {
                use std::io::Write as _;
                let _ = writeln!(f, "{arg}");
            }
        }
    }

    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
    let (update_tx, update_rx) = async_channel::unbounded::<Update>();

    // D-Bus actor on a background thread with its own single-threaded tokio runtime.
    let actor_tx = update_tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(actor(cmd_rx, actor_tx));
    });

    // The GUI's GtkApplication owns the desktop APP_ID on the session bus (drives the Wayland app_id /
    // window↔.desktop↔icon association). It must NOT be the daemon's BUS_NAME, or GApplication would try
    // to talk to ternd as if it were the primary GApplication instance and abort at registration.
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        build_ui(app, cmd_tx.clone(), update_tx.clone(), update_rx.clone())
    });
    // Run with only the program name: we've already captured any deep-link URI above, and this
    // GtkApplication uses default flags (no HANDLES_OPEN), which would otherwise error on a URI arg.
    let prog = std::env::args().next().unwrap_or_default();
    app.run_with_args(&[prog])
}

/// The background D-Bus actor: connects to `ternd`, pushes snapshots (initial + on every `Changed`), and runs
/// commands from the UI/tray.
async fn actor(cmd_rx: async_channel::Receiver<Cmd>, update_tx: async_channel::Sender<Update>) {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            let _ = update_tx.send(Update::Disconnected(format!("no session bus: {e}"))).await;
            return;
        }
    };
    let proxy = match TernProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            let _ = update_tx.send(Update::Disconnected(format!("ternd not reachable: {e}"))).await;
            return;
        }
    };

    push_snapshot(&proxy, &update_tx).await;

    {
        let proxy = proxy.clone();
        let tx = update_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = proxy.receive_changed().await {
                while let Some(signal) = stream.next().await {
                    if let Ok(args) = signal.args() {
                        if let Ok(snap) = serde_json::from_str::<Snapshot>(args.snapshot_json()) {
                            let _ = tx.send(Update::Snapshot(Box::new(snap))).await;
                        }
                    }
                }
            }
        });
    }

    while let Ok(cmd) = cmd_rx.recv().await {
        let is_refresh = matches!(cmd, Cmd::Refresh);
        let result = match cmd {
            Cmd::RedeemInvite(url) => proxy.redeem_invite(&url).await,
            Cmd::Connect(id) => proxy.connect(&id).await,
            Cmd::Disconnect => proxy.disconnect().await,
            Cmd::SignOut => proxy.sign_out().await,
            Cmd::SetAutoMount(id, on) => proxy.set_auto_mount(&id, on).await,
            Cmd::Refresh => proxy.snapshot().await,
        };
        // Surface an action failure to the user as a toast (Refresh returns a snapshot, not a result).
        if !is_refresh {
            if let Ok(json) = &result {
                if let Ok(ActionResult { ok: false, error: Some(uf) }) =
                    serde_json::from_str::<ActionResult>(json)
                {
                    let _ = update_tx.send(Update::Toast(uf.title)).await;
                }
            }
        }
        push_snapshot(&proxy, &update_tx).await;
    }
}

async fn push_snapshot(proxy: &TernProxy<'_>, tx: &async_channel::Sender<Update>) {
    if let Ok(json) = proxy.snapshot().await {
        if let Ok(snap) = serde_json::from_str::<Snapshot>(&json) {
            let _ = tx.send(Update::Snapshot(Box::new(snap))).await;
        }
    }
}

fn build_ui(
    app: &adw::Application,
    cmd_tx: async_channel::Sender<Cmd>,
    update_tx: async_channel::Sender<Update>,
    update_rx: async_channel::Receiver<Update>,
) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Tern")
        .default_width(440)
        .default_height(560)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let summary = gtk::Label::new(Some("Connecting…"));
    summary.add_css_class("title-2");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    content.append(&summary);

    // Onboarding: paste a Teleport invite (`teleport.ui.link/<uuid>`) generated in the console
    // (Settings → VPN → Teleport). Validated locally + instantly via `Invite::parse`; the pairing itself
    // goes to the daemon. This replaces the old (dead) browser-OAuth "Sign in" button (ADR-0016/docs/09).
    let invite_group = adw::PreferencesGroup::builder()
        .title("Connect a console")
        .description("Paste the Teleport invite from your console (Settings → VPN → Teleport).")
        .build();
    let invite_row = adw::EntryRow::builder().title("Teleport invite").build();
    invite_group.add(&invite_row);
    content.append(&invite_group);

    let connect_btn = gtk::Button::with_label("Connect");
    connect_btn.add_css_class("suggested-action");
    connect_btn.add_css_class("pill");
    connect_btn.set_halign(gtk::Align::Center);
    connect_btn.set_sensitive(false);
    content.append(&connect_btn);

    let access_group = adw::PreferencesGroup::new();
    access_group.set_title("Access");
    let access_row = adw::ActionRow::builder().title("One-Click VPN").subtitle("Off").build();
    let access_switch = gtk::Switch::new();
    access_switch.set_valign(gtk::Align::Center);
    access_switch.set_sensitive(false);
    access_row.add_suffix(&access_switch);
    access_group.add(&access_row);
    content.append(&access_group);

    let drives_label = gtk::Label::new(Some("Drives"));
    drives_label.add_css_class("heading");
    drives_label.set_xalign(0.0);
    content.append(&drives_label);
    let drives_list = gtk::ListBox::new();
    drives_list.add_css_class("boxed-list");
    drives_list.set_selection_mode(gtk::SelectionMode::None);
    content.append(&drives_list);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Start);
    let signout = gtk::Button::with_label("Forget this console");
    actions.append(&signout);
    content.append(&actions);

    toolbar.set_content(Some(&content));
    // An AdwToastOverlay carries transient, plain-language feedback for action results (docs/09).
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar));
    window.set_content(Some(&toast_overlay));

    // Tray icon (StatusNotifierItem). Optional: absent SNI host / no session bus → run without it.
    let tray = TernTray {
        snapshot: Snapshot::signed_out(),
        cmd_tx: cmd_tx.clone(),
        gui_tx: update_tx.clone(),
    };
    let tray_handle = match tray.spawn() {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, "tray unavailable (no SNI host?) — window still works");
            None
        }
    };

    // Guard so programmatic switch updates don't echo back as user commands.
    let updating = Rc::new(Cell::new(false));
    {
        let cmd_tx = cmd_tx.clone();
        let updating = updating.clone();
        access_switch.connect_state_set(move |_, state| {
            if !updating.get() {
                let cmd = if state { Cmd::Connect(String::new()) } else { Cmd::Disconnect };
                let _ = cmd_tx.try_send(cmd);
            }
            glib::Propagation::Proceed
        });
    }
    // Local, instant invite validation: enable Connect only when the pasted text is a valid
    // `teleport.ui.link` invite (or bare UUID); flag an obviously-wrong non-empty entry.
    {
        let connect_btn = connect_btn.clone();
        invite_row.connect_changed(move |row| {
            let text = row.text();
            let valid = !text.is_empty() && Invite::parse(text.as_str()).is_ok();
            connect_btn.set_sensitive(valid);
            if text.is_empty() || valid {
                row.remove_css_class("error");
            } else {
                row.add_css_class("error");
            }
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let invite_row = invite_row.clone();
        connect_btn.connect_clicked(move |_| {
            let _ = cmd_tx.try_send(Cmd::RedeemInvite(invite_row.text().to_string()));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        signout.connect_clicked(move |_| {
            let _ = cmd_tx.try_send(Cmd::SignOut);
        });
    }

    let app_loop = app.clone();
    let window_loop = window.clone();
    let cmd_tx_rows = cmd_tx.clone();
    glib::spawn_future_local(async move {
        let mut prev_access: Option<Access> = None;
        let mut was_expired = false;
        while let Ok(update) = update_rx.recv().await {
            match update {
                Update::Present => window_loop.present(),
                Update::Quit => app_loop.quit(),
                Update::Toast(msg) => toast_overlay.add_toast(adw::Toast::new(&msg)),
                Update::Disconnected(reason) => {
                    summary.set_text("Background service not running");
                    for w in [
                        invite_group.upcast_ref::<gtk::Widget>(),
                        connect_btn.upcast_ref(),
                        access_group.upcast_ref(),
                        drives_label.upcast_ref(),
                        drives_list.upcast_ref(),
                        actions.upcast_ref(),
                    ] {
                        w.set_visible(false);
                    }
                    access_switch.set_sensitive(false);
                    if let Some(h) = &tray_handle {
                        h.update(|t| t.snapshot = Snapshot::signed_out());
                    }
                    tracing::warn!(%reason, "actor disconnected");
                }
                Update::Snapshot(snap) => {
                    // Window is a state machine over `auth`: onboarding (paste an invite) until signed in,
                    // then the main view (Access + drives). See docs/09.
                    let signed_in = matches!(snap.auth, Auth::SignedIn(_));
                    invite_group.set_visible(!signed_in);
                    connect_btn.set_visible(!signed_in);
                    access_group.set_visible(signed_in);
                    drives_label.set_visible(signed_in);
                    drives_list.set_visible(signed_in);
                    actions.set_visible(signed_in);

                    summary.set_text(&snap.summary_line());
                    access_switch.set_sensitive(true);
                    access_row.set_subtitle(access_subtitle(snap.access));

                    let on = matches!(snap.access, Access::On | Access::TurningOn);
                    updating.set(true);
                    access_switch.set_active(on);
                    access_switch.set_state(on);
                    updating.set(false);

                    while let Some(child) = drives_list.first_child() {
                        drives_list.remove(&child);
                    }
                    for d in &snap.drives {
                        let row = adw::ActionRow::builder()
                            .title(&d.drive.name)
                            .subtitle(d.state.label())
                            .build();
                        // Auto-mount toggle. set_active BEFORE connecting so the programmatic set doesn't
                        // fire as a user action (and echo back a redundant command).
                        let sw = gtk::Switch::new();
                        sw.set_valign(gtk::Align::Center);
                        sw.set_active(d.selected);
                        let cmd_tx = cmd_tx_rows.clone();
                        let id = d.drive.id.clone();
                        sw.connect_state_set(move |_, state| {
                            let _ = cmd_tx.try_send(Cmd::SetAutoMount(id.clone(), state));
                            glib::Propagation::Proceed
                        });
                        row.add_suffix(&sw);
                        drives_list.append(&row);
                    }

                    if let Some(h) = &tray_handle {
                        let s = (*snap).clone();
                        h.update(move |t| t.snapshot = s);
                    }

                    // Notifications for the few states worth acting on (docs/05 §6).
                    let expired = matches!(snap.auth, Auth::SessionExpired);
                    if expired && !was_expired {
                        notify("Session expired", "Sign in again to stay connected.");
                    }
                    was_expired = expired;
                    // Desktop notifications (freedesktop / KDE) for every meaningful Access transition.
                    // Kept deliberately, overriding the design's "quiet by default" (owner request).
                    if let Some(prev) = prev_access {
                        if snap.access != prev {
                            match snap.access {
                                Access::On => notify("Connected", "You can reach your network."),
                                Access::Off => notify("Disconnected", "Access is off."),
                                Access::Degraded => notify("Access isn't working", "Reconnecting may help."),
                                Access::Unreachable => {
                                    notify("Can't reach your network", "Try again in a moment.")
                                }
                                Access::TurningOn => {}
                            }
                        }
                    }
                    prev_access = Some(snap.access);
                }
            }
        }
    });

    let _ = cmd_tx.try_send(Cmd::Refresh);
    window.present();
}

fn notify(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("Tern")
        .show();
}

fn access_subtitle(a: Access) -> &'static str {
    match a {
        Access::Off => "Off",
        Access::TurningOn => "Turning on…",
        Access::On => "On",
        Access::Degraded => "On, but not working",
        Access::Unreachable => "Can't reach your network",
    }
}
