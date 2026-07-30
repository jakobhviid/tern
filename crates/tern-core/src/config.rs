//! User configuration (persisted as TOML under the platform config dir). Holds *preferences*, never
//! secrets — tokens/keys/credentials live in the keyring via [`crate::backend::SecretStore`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Turn Access on automatically at login ("Connect to VPN at startup").
    pub connect_at_startup: bool,
    /// Re-establish Access automatically if it drops.
    pub auto_reconnect: bool,
    /// Ids of drives the user chose to auto-mount (the selective per-drive feature).
    pub auto_mount_drives: Vec<String>,
    /// Preferred console/site id when the account has several.
    pub preferred_console: Option<String>,
}

/// Path to the config file (`<config-dir>/tern/config.toml`), if a home/config dir is resolvable.
pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("phd", "hviid", "tern")
        .map(|d| d.config_dir().join("config.toml"))
}

impl Config {
    /// Load from disk, falling back to defaults on any error (missing file, parse error).
    pub fn load() -> Config {
        let Some(path) = config_path() else {
            return Config::default();
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk, creating the config dir if needed.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("no config directory available"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Toggle a drive's membership in the auto-mount set.
    pub fn set_auto_mount(&mut self, drive_id: &str, on: bool) {
        let present = self.auto_mount_drives.iter().any(|d| d == drive_id);
        match (on, present) {
            (true, false) => self.auto_mount_drives.push(drive_id.to_string()),
            (false, true) => self.auto_mount_drives.retain(|d| d != drive_id),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let c = Config {
            connect_at_startup: true,
            auto_reconnect: true,
            auto_mount_drives: vec!["d1".into(), "d2".into()],
            preferred_console: Some("c1".into()),
        };
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn defaults_are_conservative() {
        let c = Config::default();
        assert!(!c.connect_at_startup);
        assert!(!c.auto_reconnect);
        assert!(c.auto_mount_drives.is_empty());
    }

    #[test]
    fn set_auto_mount_is_idempotent() {
        let mut c = Config::default();
        c.set_auto_mount("d1", true);
        c.set_auto_mount("d1", true);
        assert_eq!(c.auto_mount_drives, vec!["d1".to_string()]);
        c.set_auto_mount("d1", false);
        assert!(c.auto_mount_drives.is_empty());
    }
}
