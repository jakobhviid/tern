//! WireGuard tunnel lifecycle via NetworkManager (`nmcli`). The per-session config is imported as a
//! **user-owned** connection so the logged-in desktop user can toggle it without a password (ADR-0004).
//!
//! The device private key touches disk only briefly: it's written to a `0600` file under `$XDG_RUNTIME_DIR`
//! (tmpfs), imported by NM (which then stores it in its own root-protected connection), and deleted.

use async_trait::async_trait;
use tern_core::backend::VpnBackend;
use tern_core::model::WireguardConfig;
use tern_core::{Error, Result};

use crate::cmd;

const CONNECTION: &str = "tern";

pub struct NmVpn;

impl NmVpn {
    pub fn new() -> Self {
        NmVpn
    }
}

impl Default for NmVpn {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VpnBackend for NmVpn {
    async fn connect(&self, cfg: &WireguardConfig) -> Result<()> {
        let conf = cfg
            .to_wg_quick()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("device private key not available")))?;

        // Remove any stale profile so the re-import always reflects the fresh per-session config.
        let _ = cmd::status_ok("nmcli", &["connection", "delete", CONNECTION]).await;

        let path = write_private_conf(&conf)?;
        let import = cmd::run(
            "nmcli",
            &["connection", "import", "type", "wireguard", "file", &path],
        )
        .await;
        // Remove the on-disk key material regardless of the import outcome.
        let _ = std::fs::remove_file(&path);
        import?;

        // Make it user-owned + manual so a normal desktop user can toggle it without a password.
        if let Ok(user) = std::env::var("USER") {
            let perm = format!("user:{user}");
            let _ = cmd::run(
                "nmcli",
                &[
                    "connection", "modify", CONNECTION,
                    "connection.permissions", &perm,
                    "connection.autoconnect", "no",
                ],
            )
            .await;
        }

        cmd::run("nmcli", &["connection", "up", CONNECTION]).await.map_err(|e| match e {
            Error::NetworkManagerMissing => e,
            other => {
                tracing::warn!(error = %other, "nmcli connection up failed");
                Error::VpnUnreachable
            }
        })?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        let _ = cmd::status_ok("nmcli", &["connection", "down", CONNECTION]).await;
        // Delete the profile so the per-session key doesn't linger.
        let _ = cmd::status_ok("nmcli", &["connection", "delete", CONNECTION]).await;
        Ok(())
    }

    async fn is_active(&self) -> Result<bool> {
        let out = cmd::run("nmcli", &["-t", "-f", "NAME", "connection", "show", "--active"]).await?;
        Ok(out.lines().any(|l| l == CONNECTION))
    }
}

/// Write the wg-quick config to a private (`0700` dir, `0600` file) path under the runtime dir.
fn write_private_conf(conf: &str) -> Result<String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{base}/tern");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(io_err)?;
    let path = format!("{dir}/{CONNECTION}.conf");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(io_err)?;
    f.write_all(conf.as_bytes()).map_err(io_err)?;
    Ok(path)
}

fn io_err(e: std::io::Error) -> Error {
    Error::Other(anyhow::anyhow!("writing temporary wg config: {e}"))
}
