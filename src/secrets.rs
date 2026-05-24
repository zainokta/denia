use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::command::{CommandError, CommandRunner};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecretRefError {
    #[error("secret reference cannot be empty")]
    Empty,
    #[error("secret reference must contain only ASCII letters, digits, dot, underscore, or dash")]
    InvalidCharacters,
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
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(SecretRefError::InvalidCharacters);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPayload {
    pub value: String,
}

impl SecretPayload {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SecretError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
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

    pub async fn decrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        secret_ref: &SecretRef,
    ) -> Result<SecretPayload, SecretError> {
        let secret_path = self.secret_path(secret_ref);
        let secret_path = secret_path.to_string_lossy();
        let sops_binary = sops_binary.to_string_lossy();
        let output = runner
            .run(&sops_binary, &["--decrypt", secret_path.as_ref()])
            .await?;

        Ok(serde_json::from_str(&output.stdout)?)
    }
}
