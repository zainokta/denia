use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::command::{CommandError, CommandRunner};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

    fn file_stem(&self) -> String {
        self.0
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                    byte as char
                } else {
                    '_'
                }
            })
            .collect()
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
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
    #[error("secret path traversal detected")]
    PathTraversal,
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
        let secrets_dir = self.data_dir.join("secrets");
        let candidate = secrets_dir.join(format!("{}.sops.yaml", secret_ref.file_stem()));
        candidate
    }

    fn validate_secret_path(&self, path: &std::path::Path) -> Result<(), SecretError> {
        let secrets_dir = self.data_dir.join("secrets");
        match (secrets_dir.canonicalize(), path.canonicalize()) {
            (Ok(canonical_dir), Ok(canonical_path)) => {
                if !canonical_path.starts_with(&canonical_dir) {
                    return Err(SecretError::PathTraversal);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub async fn decrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        secret_ref: &SecretRef,
    ) -> Result<SecretPayload, SecretError> {
        let secret_path = self.secret_path(secret_ref);
        self.validate_secret_path(&secret_path)?;
        let secret_path = secret_path.to_string_lossy();
        let sops_binary = sops_binary.to_string_lossy();
        let output = runner
            .run(&sops_binary, &["--decrypt", secret_path.as_ref()])
            .await?;

        Ok(serde_json::from_str(&output.stdout)?)
    }
}
