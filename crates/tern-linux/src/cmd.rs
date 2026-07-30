//! Small helpers for running the backend CLIs as subprocesses, with sensible error mapping.

use std::process::Stdio;

use tern_core::{Error, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Run a command, returning trimmed stdout on success; non-zero exit and a missing binary become errors.
pub async fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| map_spawn_err(program, e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(Error::Other(anyhow::anyhow!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Run a command and report only whether it succeeded (for probes where non-zero isn't an error).
pub async fn status_ok(program: &str, args: &[&str]) -> Result<bool> {
    match Command::new(program).args(args).output().await {
        Ok(o) => Ok(o.status.success()),
        Err(e) => Err(map_spawn_err(program, e)),
    }
}

/// Run a command, feeding `input` to its stdin (used for `secret-tool store`).
pub async fn run_with_stdin(program: &str, args: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| map_spawn_err(program, e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await.ok();
        // Drop stdin to signal EOF.
    }
    let output = child.wait_with_output().await.map_err(|e| map_spawn_err(program, e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Other(anyhow::anyhow!(
            "`{program}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn map_spawn_err(program: &str, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        if program == "nmcli" {
            return Error::NetworkManagerMissing;
        }
        return Error::Other(anyhow::anyhow!("`{program}` is not installed"));
    }
    Error::Other(anyhow::anyhow!("failed to run `{program}`: {e}"))
}
