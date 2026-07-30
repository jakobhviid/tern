//! SMB drive mounting via GVfs (`gio mount smb://…`). Userspace, no root, native Files integration, and it
//! keeps GPL-3 `libsmbclient` in the out-of-process `gvfsd-smb` daemon (ADR-0005/0007). We track which of our
//! drives are mounted in memory (keyed by drive id → URL), which is enough for the long-lived daemon.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use tern_core::backend::MountBackend;
use tern_core::model::Drive;
use tern_core::{Error, Result};

use crate::cmd;

#[derive(Default)]
pub struct GvfsMounts {
    /// drive id → smb url we mounted for it.
    mounted: Mutex<HashMap<String, String>>,
}

impl GvfsMounts {
    pub fn new() -> Self {
        Self::default()
    }
}

fn smb_url(share: &str) -> String {
    if share.starts_with("smb://") {
        share.to_string()
    } else if let Some(rest) = share.strip_prefix("//") {
        format!("smb://{rest}")
    } else {
        format!("smb://{share}")
    }
}

#[async_trait]
impl MountBackend for GvfsMounts {
    async fn mount(&self, drive: &Drive) -> Result<()> {
        let share = drive.share.as_deref().ok_or(Error::DriveUnreachable)?;
        let url = smb_url(share);
        // `gio mount` is non-interactive. If the share needs credentials this fails; upstream then prompts.
        // TODO(bazzite): feed file-service credentials from the keyring for authenticated shares.
        cmd::run("gio", &["mount", &url]).await.map_err(|e| {
            tracing::warn!(error = %e, url, "gio mount failed");
            Error::DriveMountFailed(url.clone())
        })?;
        self.mounted.lock().unwrap().insert(drive.id.clone(), url);
        Ok(())
    }

    async fn unmount(&self, drive: &Drive) -> Result<()> {
        let url = { self.mounted.lock().unwrap().get(&drive.id).cloned() };
        if let Some(url) = url {
            let _ = cmd::status_ok("gio", &["mount", "-u", &url]).await;
        }
        self.mounted.lock().unwrap().remove(&drive.id);
        Ok(())
    }

    async fn mounted(&self) -> Result<Vec<String>> {
        Ok(self.mounted.lock().unwrap().keys().cloned().collect())
    }
}
