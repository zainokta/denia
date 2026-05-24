use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecretRefError {
    #[error("secret reference cannot be empty")]
    Empty,
}

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, SecretRefError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SecretRefError::Empty);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SopsSecretStore {
    data_dir: std::path::PathBuf,
}

impl SopsSecretStore {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn secret_path(&self, secret_ref: &SecretRef) -> std::path::PathBuf {
        self.data_dir
            .join("secrets")
            .join(format!("{}.sops.yaml", secret_ref.as_str()))
    }
}
