//! `ternd` — the background service. Owns the [`Engine`] (session + orchestration) and exposes it on the
//! session bus for the tray/GUI/CLI clients (see `service`). Runs as a systemd `--user` service on native
//! installs; under Flatpak it's spawned by the app and kept alive via the Background portal (ADR-0002).
//!
//! Backend selection: on Linux it will use the real NetworkManager/GVfs/keyring backends from `tern-linux`
//! (M4); until those land — and on non-Linux build hosts — it falls back to the in-memory stub so the IPC and
//! GUI can be developed and exercised first.

use std::sync::Arc;

use tern_core::config::Config;
use tern_core::engine::Engine;
use tern_core::ipc::{BUS_NAME, OBJECT_PATH};
use tern_core::ucs::{Endpoints, UcsClient};
use tokio::sync::Mutex;

mod service;
use service::TernService;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ternd=info,tern_core=info".into()),
        )
        .init();

    // Raise CAP_NET_ADMIN into inheritable+ambient BEFORE building the tokio runtime — ambient capabilities
    // are per-thread, so raising here (on the main thread) means the runtime's worker threads inherit it when
    // they're created. `on_thread_start` re-raises on every worker/blocking thread as belt-and-suspenders, so
    // whichever thread ends up fork/exec'ing `ip`/`sysctl`/`resolvectl` carries the capability.
    #[cfg(target_os = "linux")]
    raise_ambient_net_admin();

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    #[cfg(target_os = "linux")]
    builder.on_thread_start(|| {
        use caps::{CapSet, Capability};
        let _ = caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_ADMIN);
        let _ = caps::raise(None, CapSet::Ambient, Capability::CAP_NET_ADMIN);
    });
    builder.build()?.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let engine = Arc::new(Mutex::new(build_engine()));

    // Restore a saved session at startup, and honour "connect at startup" (before clients connect, so no
    // signal emission is needed — the first snapshot they read reflects the result).
    {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut e = engine.lock().await;
            // Restore whichever session is stored: the account SSO token and/or a redeemed Teleport session
            // (ADR-0016). Either lets the Access toggle reconnect; `connect_at_startup` brings it up now.
            let account = e.restore_session().await.unwrap_or(false);
            let teleport = e.restore_teleport_session().await.unwrap_or(false);
            if account || teleport {
                tracing::info!(account, teleport, "restored saved session");
                if e.config().connect_at_startup {
                    if let Err(err) = e.connect("").await {
                        tracing::warn!(error = %err, "connect-at-startup failed");
                    }
                }
            } else {
                tracing::info!("no saved session");
            }
        });
    }

    let service = TernService::new(engine);

    let _conn = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;

    tracing::info!(bus = BUS_NAME, path = OBJECT_PATH, "ternd started");

    // Serve until asked to stop.
    tokio::signal::ctrl_c().await?;
    tracing::info!("ternd shutting down");
    Ok(())
}

/// Raise `CAP_NET_ADMIN` into the **ambient** capability set so the helpers the Teleport backend execs
/// (`ip`/`sysctl`/`resolvectl`) inherit it. The daemon carries the capability as a *file* capability
/// (`setcap cap_net_admin+eip ternd`); that grants it to `ternd` itself (permitted+effective), but when a
/// file-capability binary is exec'd the kernel leaves the *inheritable* set empty and clears ambient — so we
/// must add `CAP_NET_ADMIN` to inheritable first (allowed because it's in our permitted set), then raise it
/// into ambient. Best-effort: without the capability the tunnel reports "privilege required" rather than the
/// daemon failing to start.
#[cfg(target_os = "linux")]
fn raise_ambient_net_admin() {
    use caps::{CapSet, Capability};
    let cap = Capability::CAP_NET_ADMIN;
    let inh = caps::raise(None, CapSet::Inheritable, cap);
    let amb = inh.as_ref().ok().and(Some(())).map(|_| caps::raise(None, CapSet::Ambient, cap));
    match (inh, amb) {
        (Ok(()), Some(Ok(()))) => tracing::info!("raised CAP_NET_ADMIN into inheritable+ambient for tunnel helpers"),
        (Err(e), _) => tracing::info!(error = %e, "CAP_NET_ADMIN not permitted — run `setcap cap_net_admin+eip` on ternd to enable the tunnel"),
        (Ok(()), amb) => tracing::info!(?amb, "could not raise CAP_NET_ADMIN into the ambient set"),
    }
}

/// Construct the engine with the appropriate backends for this build target: the real
/// NetworkManager/GVfs/keyring backends on Linux, the in-memory stub elsewhere (macOS/CI build hosts).
fn build_engine() -> Engine {
    let ucs = UcsClient::new(Endpoints::default());
    let config = Config::load();

    #[cfg(target_os = "linux")]
    {
        let (vpn, teleport, mounts, reach, secrets) = tern_linux::backends();
        Engine::new(ucs, vpn, teleport, mounts, reach, secrets, config)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let stub = Arc::new(tern_core::backend::StubBackend::new());
        Engine::new(ucs, stub.clone(), stub.clone(), stub.clone(), stub.clone(), stub, config)
    }
}
