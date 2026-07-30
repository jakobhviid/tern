//! The session-bus D-Bus interface for `ternd`. Wire contract (see `tern_core::ipc`): every method returns a
//! JSON string — either a [`tern_core::state::Snapshot`] or an `{ok, error}` envelope where `error` is the
//! plain-language [`tern_core::error::UserFacing`] — and a `Changed` signal carries the new snapshot JSON so
//! the tray/GUI update live rather than polling.

use std::sync::Arc;

use serde::Serialize;
use tern_core::engine::Engine;
use tern_core::error::UserFacing;
use tokio::sync::Mutex;
use zbus::interface;
use zbus::object_server::SignalEmitter;

pub struct TernService {
    engine: Arc<Mutex<Engine>>,
}

/// The `{ok, error}` envelope returned by action methods.
#[derive(Serialize)]
struct ActionResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<UserFacing>,
}

impl TernService {
    pub fn new(engine: Arc<Mutex<Engine>>) -> Self {
        Self { engine }
    }

    async fn snapshot_json(&self) -> String {
        let e = self.engine.lock().await;
        serde_json::to_string(&e.snapshot().await).unwrap_or_else(|_| "{}".to_string())
    }

    async fn emit_changed(&self, emitter: &SignalEmitter<'_>) {
        let json = self.snapshot_json().await;
        let _ = Self::changed(emitter, json).await;
    }

    /// Turn an engine result into the JSON envelope, and notify clients of the new state.
    async fn finish(&self, res: tern_core::Result<()>, emitter: &SignalEmitter<'_>) -> String {
        let out = match res {
            Ok(()) => ActionResult { ok: true, error: None },
            Err(e) => ActionResult { ok: false, error: Some(e.user_facing()) },
        };
        self.emit_changed(emitter).await;
        serde_json::to_string(&out).unwrap_or_else(|_| r#"{"ok":false}"#.to_string())
    }
}

#[interface(name = "phd.hviid.Tern")]
impl TernService {
    /// Current UI snapshot as JSON.
    async fn snapshot(&self) -> String {
        self.snapshot_json().await
    }

    /// Consoles/sites available to the signed-in user, as a JSON array.
    async fn hosts(&self) -> String {
        let e = self.engine.lock().await;
        serde_json::to_string(e.hosts()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Complete sign-in with a bearer token obtained from browser SSO (placeholder until the loopback flow
    /// lands — see ADR-0009).
    async fn complete_sign_in(
        &self,
        token: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> String {
        let res = self.engine.lock().await.sign_in(token).await;
        self.finish(res, &emitter).await
    }

    async fn sign_out(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> String {
        let res = self.engine.lock().await.sign_out().await;
        self.finish(res, &emitter).await
    }

    /// Turn on Access for a console.
    async fn connect(
        &self,
        console_id: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> String {
        let res = self.engine.lock().await.connect(&console_id).await;
        self.finish(res, &emitter).await
    }

    async fn disconnect(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> String {
        let res = self.engine.lock().await.disconnect().await;
        self.finish(res, &emitter).await
    }

    /// Toggle whether a drive auto-mounts, persist the choice, and re-evaluate mounts.
    async fn set_auto_mount(
        &self,
        drive_id: String,
        on: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> String {
        {
            let mut e = self.engine.lock().await;
            e.config_mut().set_auto_mount(&drive_id, on);
            let _ = e.config().save();
            e.mount_selected().await;
        }
        self.emit_changed(&emitter).await;
        serde_json::to_string(&ActionResult { ok: true, error: None })
            .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
    }

    /// Emitted whenever state changes; carries the new [`Snapshot`] as JSON.
    #[zbus(signal)]
    async fn changed(emitter: &SignalEmitter<'_>, snapshot_json: String) -> zbus::Result<()>;
}
