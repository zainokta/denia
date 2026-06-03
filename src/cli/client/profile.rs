//! Client profile storage: named (url, token) profiles in a user config file
//! written owner-only where the platform supports it. See ADR-030.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("profile io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile TOML decode error: {0}")]
    Decode(#[from] toml::de::Error),
    #[error("profile TOML encode error: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("active profile not found: {0}")]
    ActiveMissing(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub active: String,
    pub profiles: Vec<ClientProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientProfile {
    pub name: String,
    pub url: String,
    pub token: String,
}

impl ClientConfig {
    pub fn load_from(path: &Path) -> Result<Self, ProfileError> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ProfileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_owner_only(path, toml::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }

    pub fn active_profile(&self) -> Result<&ClientProfile, ProfileError> {
        self.profiles
            .iter()
            .find(|p| p.name == self.active)
            .ok_or_else(|| ProfileError::ActiveMissing(self.active.clone()))
    }
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
