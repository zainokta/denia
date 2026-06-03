//! `.denia` project manifest: committed, secret-free deploy settings parsed by
//! `denia push`. See ADR-030.

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid .denia TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("unsupported .denia version: {0}")]
    UnsupportedVersion(u32),
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("health.path must start with /")]
    InvalidHealthPath,
    #[error("runtime.internal_port must be between 1 and 65535")]
    InvalidPort,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeniaManifest {
    pub version: u32,
    pub project: String,
    pub service: String,
    pub source: GitSourceManifest,
    pub runtime: RuntimeManifest,
    pub health: HealthManifest,
    #[serde(default)]
    pub limits: Option<LimitsManifest>,
    #[serde(default)]
    pub ingress: IngressManifest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitSourceManifest {
    #[serde(rename = "type")]
    pub source_type: String,
    pub remote: String,
    pub dockerfile: String,
    pub context: String,
    pub git_credential_ref: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeManifest {
    pub internal_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthManifest {
    pub path: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsManifest {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct IngressManifest {
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub tls_enabled: bool,
}

impl DeniaManifest {
    pub fn parse(raw: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.version != 1 {
            return Err(ManifestError::UnsupportedVersion(self.version));
        }
        non_empty("project", &self.project)?;
        non_empty("service", &self.service)?;
        if self.source.source_type != "git" {
            return Err(ManifestError::Empty("source.type must be git"));
        }
        non_empty("source.remote", &self.source.remote)?;
        non_empty("source.dockerfile", &self.source.dockerfile)?;
        non_empty("source.context", &self.source.context)?;
        non_empty("source.git_credential_ref", &self.source.git_credential_ref)?;
        if self.runtime.internal_port == 0 {
            return Err(ManifestError::InvalidPort);
        }
        if !self.health.path.starts_with('/') {
            return Err(ManifestError::InvalidHealthPath);
        }
        Ok(())
    }
}

fn non_empty(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        Err(ManifestError::Empty(field))
    } else {
        Ok(())
    }
}
