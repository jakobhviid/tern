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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ternd=info,tern_core=info".into()),
        )
        .init();

    let engine = Arc::new(Mutex::new(build_engine()));

    // Restore a saved session at startup, and honour "connect at startup" (before clients connect, so no
    // signal emission is needed — the first snapshot they read reflects the result).
    {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut e = engine.lock().await;
            match e.restore_session().await {
                Ok(true) => {
                    tracing::info!("restored saved session");
                    if e.config().connect_at_startup {
                        if let Err(err) = e.connect("").await {
                            tracing::warn!(error = %err, "connect-at-startup failed");
                        }
                    }
                }
                Ok(false) => tracing::info!("no saved session"),
                Err(err) => tracing::warn!(error = %err, "restoring session failed"),
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

/// Construct the engine with the appropriate backends for this build target: the real
/// NetworkManager/GVfs/keyring backends on Linux, the in-memory stub elsewhere (macOS/CI build hosts).
fn build_engine() -> Engine {
    let ucs = UcsClient::new(Endpoints::default());
    let config = Config::load();

    #[cfg(target_os = "linux")]
    {
        let (vpn, mounts, reach, secrets) = tern_linux::backends();
        Engine::new(ucs, vpn, mounts, reach, secrets, config)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let stub = Arc::new(tern_core::backend::StubBackend::new());
        Engine::new(ucs, stub.clone(), stub.clone(), stub.clone(), stub, config)
    }
}
