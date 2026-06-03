//! Cross-platform client configuration: named profiles (`url` + bearer `token`)
//! used by client subcommands like `denia console` to reach a remote Denia
//! control plane. Distinct from the daemon's own config (ADR-023): this is the
//! operator-workstation credential store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ClientConfig {
    /// Name of the profile to use when a command does not pick one explicitly.
    pub active: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl ClientConfig {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!(
                "no client profile configured at {} ({error}); set DENIA_CLIENT_CONFIG or create the file",
                path.display()
            )
        })?;
        let config: ClientConfig = toml::from_str(&raw).map_err(|error| {
            anyhow::anyhow!(
                "invalid client profile config at {}: {error}",
                path.display()
            )
        })?;
        Ok(config)
    }

    /// Resolve the profile to use: the `active` one if set, otherwise the sole
    /// profile when exactly one is configured.
    pub fn active_profile(&self) -> anyhow::Result<&Profile> {
        if let Some(name) = &self.active {
            return self
                .profiles
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("active client profile '{name}' not found"));
        }
        match self.profiles.len() {
            1 => Ok(self.profiles.values().next().expect("one profile")),
            0 => anyhow::bail!("no client profile configured; set DENIA_CLIENT_CONFIG"),
            _ => anyhow::bail!(
                "multiple client profiles configured but none is active; set `active`"
            ),
        }
    }

    /// Insert or replace a named profile.
    pub fn upsert_profile(&mut self, name: &str, profile: Profile) {
        self.profiles.insert(name.to_string(), profile);
    }

    /// Set the active profile by name.
    pub fn set_active(&mut self, name: &str) {
        self.active = Some(name.to_string());
    }

    /// Atomically write the config to `path` with 0o600 permissions.
    ///
    /// Creates parent directories as needed, writes to a `.toml.tmp` sibling
    /// first, then renames into place.
    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_set_active_and_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.toml");
        let mut cfg = ClientConfig::default();
        cfg.upsert_profile("prod", Profile { url: "https://x".into(), token: "t".into() });
        cfg.set_active("prod");
        cfg.save_to(&path).unwrap();
        let back = ClientConfig::load_from(&path).unwrap();
        assert_eq!(back.active.as_deref(), Some("prod"));
        assert_eq!(back.active_profile().unwrap().token, "t");
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_owner_only_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.toml");
        let mut cfg = ClientConfig::default();
        cfg.upsert_profile("p", Profile { url: "u".into(), token: "t".into() });
        cfg.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

/// Path to the client config file. Honors `DENIA_CLIENT_CONFIG`, else falls back
/// to `$XDG_CONFIG_HOME/denia/client.toml` (or `$HOME/.config/denia/client.toml`).
pub fn config_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("DENIA_CLIENT_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            anyhow::anyhow!("cannot determine config directory; set DENIA_CLIENT_CONFIG")
        })?;
    Ok(base.join("denia").join("client.toml"))
}
