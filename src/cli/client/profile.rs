//! Cross-platform client configuration: named profiles (`url` + bearer `token`)
//! used by client subcommands like `denia console` to reach a remote Denia
//! control plane. Distinct from the daemon's own config (ADR-023): this is the
//! operator-workstation credential store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Default, Deserialize)]
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
