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

    /// Current on-disk size of an in-progress upload's data file, or 0 if it
    /// does not exist yet. Used to enforce a cumulative size cap across PATCH
    /// chunks while streaming so a single upload cannot exceed the configured
    /// maximum blob size.
    pub fn upload_size(&self, upload_id: Uuid) -> Result<u64, RegistryStorageError> {
        let path = self.upload_data_path(upload_id);
        match std::fs::metadata(&path) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(RegistryStorageError::Io(e)),
        }
    }

    pub fn hash_upload(&self, upload_id: Uuid) -> Result<(String, u64), RegistryStorageError> {
        let path = self.upload_data_path(upload_id);
        // A monolithic commit (start → empty PATCH → PUT?digest=) or an empty
        // blob may never have created a data file. Treat a missing file as a
        // zero-byte upload so the empty-blob digest is computed correctly.
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"")));
                return Ok((digest, 0));
            }
            Err(e) => return Err(RegistryStorageError::Io(e)),
        };
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
        let size = match std::fs::metadata(&src) {
            Ok(m) => {
                // fsync the data file before rename so a post-crash blob is
                // either absent or fully durable under its content-addressed
                // name. Without this a crash can leave a half-written or
                // zero-length file filed under a digest the metadata claims is
                // verified (matches the OCI cache durability in
                // `oci/cache/store.rs::finalize_temp`).
                {
                    let f = std::fs::File::open(&src)?;
                    f.sync_all()?;
                }
                std::fs::rename(&src, &dst)?;
                m.len()
            }
            // A zero-byte / monolithic-empty upload may never have created a
            // data file. Materialise an empty blob durably under its digest.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let f = std::fs::File::create(&dst)?;
                f.sync_all()?;
                0
            }
            Err(e) => return Err(RegistryStorageError::Io(e)),
        };
        sync_parent_dir(&dst);
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

    /// Write `bytes` to the content-addressed path for `digest`, atomically
    /// and durably: stage into a sibling `.tmp` file, fsync it, rename into
    /// place, then fsync the parent directory. A crash therefore leaves the
    /// blob either absent or fully written — never a half-written file under
    /// a verified digest. Returns the number of bytes written.
    pub fn put_content(&self, digest: &str, bytes: &[u8]) -> Result<u64, RegistryStorageError> {
        let path = self.blob_path(digest)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        sync_parent_dir(&path);
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

/// Best-effort fsync of the directory holding `path` so a freshly created or
/// renamed entry is durable across a crash. Directory fsync is advisory on
/// some filesystems; failures are ignored because the data file itself was
/// already fsynced before the rename.
fn sync_parent_dir(path: &std::path::Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn fresh() -> (tempfile::TempDir, RegistryStorage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = RegistryStorage::new(dir.path().to_path_buf());
        (dir, storage)
    }

    #[test]
    fn upload_size_tracks_appends() {
        let (_g, storage) = fresh();
        let upload = Uuid::now_v7();
        assert_eq!(storage.upload_size(upload).unwrap(), 0);
        storage.create_upload(upload).unwrap();
        storage.append_upload(upload, b"abc").unwrap();
        assert_eq!(storage.upload_size(upload).unwrap(), 3);
        storage.append_upload(upload, b"defg").unwrap();
        assert_eq!(storage.upload_size(upload).unwrap(), 7);
    }

    #[test]
    fn commit_blob_is_durable_and_content_addressed() {
        let (_g, storage) = fresh();
        let upload = Uuid::now_v7();
        let bytes = b"layer-payload";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        storage.create_upload(upload).unwrap();
        storage.append_upload(upload, bytes).unwrap();
        let size = storage.commit_blob(upload, &digest).unwrap();
        assert_eq!(size, bytes.len() as u64);
        // The upload dir is cleaned up; the blob is readable under its digest.
        assert!(!storage.upload_dir(upload).exists());
        assert_eq!(storage.read_blob(&digest).unwrap(), bytes);
    }

    #[test]
    fn put_content_leaves_no_tmp_and_is_atomic() {
        let (_g, storage) = fresh();
        let bytes = b"{\"manifest\":true}";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        let written = storage.put_content(&digest, bytes).unwrap();
        assert_eq!(written, bytes.len() as u64);
        let final_path = storage.blob_path(&digest).unwrap();
        assert!(final_path.exists());
        // The staging `.tmp` sibling must have been renamed away.
        assert!(!final_path.with_extension("tmp").exists());
        assert_eq!(storage.read_blob(&digest).unwrap(), bytes);
    }
}
