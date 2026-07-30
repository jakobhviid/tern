//! Secret storage via the GNOME keyring / Secret Service, driven through `secret-tool` (part of libsecret).
//! Stores tokens, the wg private key, and SMB credentials under attributes `{application: tern, key: <name>}`.
//! (A native `oo7`/libsecret backend — portal-aware for Flatpak — is a noted future refinement.)

use async_trait::async_trait;
use tern_core::backend::SecretStore;
use tern_core::Result;

use crate::cmd;

const APP: &str = "tern";

pub struct KeyringSecrets;

impl KeyringSecrets {
    pub fn new() -> Self {
        KeyringSecrets
    }
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretStore for KeyringSecrets {
    async fn set(&self, key: &str, value: &str) -> Result<()> {
        let label = format!("tern: {key}");
        cmd::run_with_stdin(
            "secret-tool",
            &["store", "--label", &label, "application", APP, "key", key],
            value,
        )
        .await
    }

    async fn get(&self, key: &str) -> Result<Option<String>> {
        // `secret-tool lookup` exits non-zero when the item isn't found; treat that as "no value".
        match cmd::run("secret-tool", &["lookup", "application", APP, "key", key]).await {
            Ok(s) if !s.is_empty() => Ok(Some(s)),
            _ => Ok(None),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let _ = cmd::status_ok("secret-tool", &["clear", "application", APP, "key", key]).await;
        Ok(())
    }
}
