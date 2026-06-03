use sha2::{Digest, Sha256};
use std::io::{Read, Write};
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
        Self {
            root: data_dir.join("registry"),
        }
    }

    pub fn blob_path(&self, digest: &str) -> Result<PathBuf, RegistryStorageError> {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or(RegistryStorageError::InvalidDigest)?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RegistryStorageError::InvalidDigest);
        }
        Ok(self.root.join("blobs").join("sha256").join(hex))
    }

    pub fn upload_dir(&self, upload_id: Uuid) -> PathBuf {
        self.root.join("uploads").join(upload_id.to_string())
    }

    pub fn upload_data_path(&self, upload_id: Uuid) -> PathBuf {
        self.upload_dir(upload_id).join("data")
    }

    pub fn create_upload(&self, upload_id: Uuid) -> Result<PathBuf, RegistryStorageError> {
        let dir = self.upload_dir(upload_id);
        std::fs::create_dir_all(&dir)?;
        Ok(self.upload_data_path(upload_id))
    }

    pub fn append_upload(
        &self,
        upload_id: Uuid,
        bytes: &[u8],
    ) -> Result<u64, RegistryStorageError> {
        let path = self.upload_data_path(upload_id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(bytes)?;
        file.flush()?;
        let len = file.metadata()?.len();
        Ok(len)
    }

    pub fn hash_upload(&self, upload_id: Uuid) -> Result<(String, u64), RegistryStorageError> {
        let path = self.upload_data_path(upload_id);
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 8192];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        let digest = format!("sha256:{}", hex::encode(hasher.finalize()));
        Ok((digest, total))
    }

    pub fn commit_blob(&self, upload_id: Uuid, digest: &str) -> Result<u64, RegistryStorageError> {
        let dst = self.blob_path(digest)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let src = self.upload_data_path(upload_id);
        let size = std::fs::metadata(&src)?.len();
        std::fs::rename(&src, &dst)?;
        // Clean up the upload directory; ignore errors (best effort)
        let _ = std::fs::remove_dir_all(self.upload_dir(upload_id));
        Ok(size)
    }

    pub fn read_blob(&self, digest: &str) -> Result<Vec<u8>, RegistryStorageError> {
        let path = self.blob_path(digest)?;
        Ok(std::fs::read(&path)?)
    }

    pub fn blob_size(&self, digest: &str) -> Result<Option<u64>, RegistryStorageError> {
        let path = self.blob_path(digest)?;
        match std::fs::metadata(&path) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(RegistryStorageError::Io(e)),
        }
    }

    /// Write `bytes` to the content-addressed path for `digest` (atomic).
    /// Returns the number of bytes written.
    pub fn put_content(&self, digest: &str, bytes: &[u8]) -> Result<u64, RegistryStorageError> {
        let path = self.blob_path(digest)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        Ok(bytes.len() as u64)
    }

    pub fn delete_upload(&self, upload_id: Uuid) -> Result<(), RegistryStorageError> {
        let dir = self.upload_dir(upload_id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RegistryStorageError::Io(e)),
        }
    }

    /// Walk `<root>/blobs/sha256/*` and return `(digest, path, mtime, size)`
    /// for each blob file. Used by the garbage collector to enumerate
    /// candidates. Returns an empty Vec if the directory does not exist.
    pub fn list_blobs(
        &self,
    ) -> Result<Vec<(String, PathBuf, std::time::SystemTime, u64)>, RegistryStorageError> {
        let dir = self.root.join("blobs").join("sha256");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(RegistryStorageError::Io(e)),
        };
        let mut blobs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            let mtime = metadata.modified()?;
            blobs.push((
                format!("sha256:{file_name}"),
                entry.path(),
                mtime,
                metadata.len(),
            ));
        }
        Ok(blobs)
    }

    /// Count upload session directories under `<root>/uploads/` that still
    /// have a `data` file (i.e. in-progress uploads). The GC never deletes
    /// these — they live in a separate directory and never appear in
    /// [`Self::list_blobs`].
    pub fn count_active_uploads(&self) -> Result<u64, RegistryStorageError> {
        let dir = self.root.join("uploads");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(RegistryStorageError::Io(e)),
        };
        let mut count = 0u64;
        for entry in entries {
            let entry = entry?;
            if entry.path().join("data").is_file() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Remove a blob file by digest. Missing files are treated as success
    /// (idempotent delete).
    pub fn delete_blob(&self, digest: &str) -> Result<(), RegistryStorageError> {
        let path = self.blob_path(digest)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RegistryStorageError::Io(e)),
        }
    }
}
