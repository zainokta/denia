use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum RegistryStorageError {
    #[error("digest must be sha256:<hex>")]
    InvalidDigest,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct RegistryStorage {
    root: PathBuf,
}

impl RegistryStorage {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { root: data_dir.join("registry") }
    }

    pub fn blob_path(&self, digest: &str) -> Result<PathBuf, RegistryStorageError> {
        let hex = digest.strip_prefix("sha256:").ok_or(RegistryStorageError::InvalidDigest)?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RegistryStorageError::InvalidDigest);
        }
        Ok(self.root.join("blobs").join("sha256").join(hex))
    }

    pub fn upload_dir(&self, upload_id: Uuid) -> PathBuf {
        self.root.join("uploads").join(upload_id.to_string())
    }
}
