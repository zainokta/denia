use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::config::OciCacheVerifyMode;

use super::error::CacheError;

/// Filename suffix for the per-blob "last-reference time" sidecar (ADR-022).
/// Its mtime is used as the blob's atime even when the underlying fs is
/// mounted `noatime`/`relatime`.
pub const LASTREF_SUFFIX: &str = ".lastref";

/// One row of [`LayerCache::list_blobs`]: `(digest, blob_path, lastref_mtime,
/// size_bytes)`. The lastref is `None` when the sidecar is missing.
pub(crate) type BlobListEntry = (String, PathBuf, Option<SystemTime>, u64);

/// Filename suffix for an in-flight download. Atomically renamed onto the
/// final blob path after `fsync`. The GC ignores files with this suffix —
/// they are not blobs.
pub const TEMP_SUFFIX: &str = ".tmp";

/// A persistent content-addressed cache for OCI layer blobs.
///
/// Cloneable handle; concurrent access is mediated by:
/// - a coarse `RwLock` on the whole cache (the GC takes the write side to
///   walk; pulls take the read side to enter a critical section just long
///   enough to bump a reservation),
/// - a per-cache `Mutex<BTreeMap<digest, usize>>` of in-flight reservations
///   so GC can refuse to delete a digest mid-pull.
#[derive(Clone)]
pub struct LayerCache {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    verify_on_hit: OciCacheVerifyMode,
    lock: RwLock<()>,
    reservations: Mutex<BTreeMap<String, usize>>,
}

/// RAII reservation: while held, the GC will not delete the named blob.
/// Dropped at the end of every pull (success or failure).
pub struct CacheReservation {
    cache: LayerCache,
    digest: String,
    released: bool,
}

impl std::fmt::Debug for CacheReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheReservation")
            .field("digest", &self.digest)
            .field("released", &self.released)
            .finish()
    }
}

impl CacheReservation {
    /// Explicitly release the reservation before drop. Idempotent.
    pub fn release(mut self) {
        self.release_now();
    }

    fn release_now(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut map = match self.cache.inner.reservations.lock() {
            Ok(map) => map,
            Err(_) => return,
        };
        if let Some(count) = map.get_mut(&self.digest) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.digest);
            }
        }
    }
}

impl Drop for CacheReservation {
    fn drop(&mut self) {
        self.release_now();
    }
}

/// Lightweight snapshot of the cache for the observability endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStatus {
    pub entries: u64,
    pub total_bytes: u64,
    pub oldest_entry_age_secs: Option<u64>,
}

impl LayerCache {
    /// Open (and create on first use) a cache rooted at `root`. Creates
    /// `<root>/blobs/sha256/` mode `0700` so blobs inherit a parent dir
    /// that is not world-readable.
    pub fn new(root: PathBuf, verify_on_hit: OciCacheVerifyMode) -> Result<Self, CacheError> {
        let blobs = root.join("blobs").join("sha256");
        fs::create_dir_all(&blobs)?;
        let _ = fs::set_permissions(&blobs, fs::Permissions::from_mode(0o700));
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                verify_on_hit,
                lock: RwLock::new(()),
                reservations: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn verify_on_hit(&self) -> OciCacheVerifyMode {
        self.inner.verify_on_hit
    }

    /// Path where a blob with the given digest would live. Public so the GC
    /// and tests can locate blobs without re-implementing the layout.
    pub fn blob_path(&self, digest: &str) -> Result<PathBuf, CacheError> {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or_else(|| CacheError::UnsupportedDigest(digest.to_string()))?;
        sanitize_hex(hex)?;
        Ok(self.inner.root.join("blobs").join("sha256").join(hex))
    }

    /// Sidecar path for the last-reference mtime.
    pub fn lastref_path(&self, digest: &str) -> Result<PathBuf, CacheError> {
        let mut path = self.blob_path(digest)?;
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        name.push(LASTREF_SUFFIX);
        path.set_file_name(name);
        Ok(path)
    }

    /// Reserve `digest` against the GC for the lifetime of the returned
    /// guard. Cheap: a mutex-protected ref-count bump.
    pub fn reserve(&self, digest: &str) -> Result<CacheReservation, CacheError> {
        // The reservation observation must happen-after any GC walk that the
        // caller raced; the read-lock guarantees this ordering with the
        // GC's write-lock.
        let _read = self
            .inner
            .lock
            .read()
            .map_err(|_| CacheError::LockPoisoned)?;
        let mut map = self
            .inner
            .reservations
            .lock()
            .map_err(|_| CacheError::LockPoisoned)?;
        *map.entry(digest.to_string()).or_insert(0) += 1;
        Ok(CacheReservation {
            cache: self.clone(),
            digest: digest.to_string(),
            released: false,
        })
    }

    /// Number of outstanding reservations for `digest`. Used by the GC.
    pub fn reservation_count(&self, digest: &str) -> usize {
        self.inner
            .reservations
            .lock()
            .map(|m| m.get(digest).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// Returns the cached blob path if present AND it passes the configured
    /// verify-on-hit check. On full-verify failure (corruption), removes the
    /// bad blob so the caller falls through to the network path.
    ///
    /// `expected_size` is the descriptor's declared size, used by the `Size`
    /// mode. Pass `None` if the caller does not know it (Size mode then
    /// degrades to None mode for that hit).
    pub fn get(
        &self,
        digest: &str,
        expected_size: Option<u64>,
    ) -> Result<Option<PathBuf>, CacheError> {
        let path = self.blob_path(digest)?;
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match self.inner.verify_on_hit {
            OciCacheVerifyMode::None => {}
            OciCacheVerifyMode::Size => {
                if let Some(expected) = expected_size
                    && meta.len() != expected
                {
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(self.lastref_path(digest)?);
                    return Err(CacheError::SizeMismatch {
                        digest: digest.to_string(),
                        expected,
                        actual: meta.len(),
                    });
                }
            }
            OciCacheVerifyMode::Full => {
                let actual = hash_file_sha256(&path)?;
                let expected = digest
                    .strip_prefix("sha256:")
                    .ok_or_else(|| CacheError::UnsupportedDigest(digest.to_string()))?
                    .to_string();
                if actual != expected {
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(self.lastref_path(digest)?);
                    return Err(CacheError::DigestMismatch {
                        digest: digest.to_string(),
                        expected: format!("sha256:{expected}"),
                        actual: format!("sha256:{actual}"),
                    });
                }
            }
        }
        self.touch_lastref(digest)?;
        Ok(Some(path))
    }

    /// Atomically install bytes already present at `tmp_path` as the cached
    /// blob for `digest`. Caller is responsible for streaming the bytes into
    /// `tmp_path` (which must live inside the same directory) and for digest
    /// verification before calling — typically [`oci_client::Client::pull_blob`]
    /// has already streamed-and-verified. After install, the lastref sidecar
    /// is touched.
    pub fn finalize_temp(&self, digest: &str, tmp_path: &Path) -> Result<PathBuf, CacheError> {
        let final_path = self.blob_path(digest)?;
        // fsync the data file before rename so a post-crash blob is either
        // absent or fully durable.
        let f = fs::File::open(tmp_path)?;
        f.sync_all()?;
        drop(f);
        let _ = fs::set_permissions(tmp_path, fs::Permissions::from_mode(0o600));
        // Write the `.lastref` sidecar BEFORE renaming the blob into place, so
        // a freshly-installed blob is never observed by the GC without a
        // sidecar. The GC treats a missing sidecar as "expired" (see
        // `gc.rs::sweep_once`); renaming first left a window where a concurrent
        // sweep could see the blob with no sidecar and delete it (only the
        // pull's reservation saved it). Touch-then-rename closes that window.
        // An orphan sidecar (if the rename below fails) is harmless: the GC
        // only lists blob files, never bare sidecars.
        self.touch_lastref(digest)?;
        fs::rename(tmp_path, &final_path)?;
        Ok(final_path)
    }

    /// Allocate a path inside the cache to stream into. Caller writes to
    /// this path then calls [`finalize_temp`].
    pub fn temp_path(&self, digest: &str) -> Result<PathBuf, CacheError> {
        let mut path = self.blob_path(digest)?;
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        name.push(TEMP_SUFFIX);
        path.set_file_name(name);
        Ok(path)
    }

    /// Allocate a PER-PULL UNIQUE temp path (`<hex>.<uuid>.tmp`) so two
    /// concurrent pulls of the same digest never share or interleave one tmp
    /// file. The name still ends in [`TEMP_SUFFIX`], so the GC ignores it.
    /// Caller opens with `O_EXCL`/`create_new`, streams, then calls
    /// [`finalize_temp`] with this exact path.
    pub fn unique_temp_path(&self, digest: &str) -> Result<PathBuf, CacheError> {
        let mut path = self.blob_path(digest)?;
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        name.push(format!(".{}", uuid::Uuid::now_v7()));
        name.push(TEMP_SUFFIX);
        path.set_file_name(name);
        Ok(path)
    }

    /// Update the sidecar mtime to now. Creates the sidecar if missing.
    pub fn touch_lastref(&self, digest: &str) -> Result<(), CacheError> {
        let sidecar = self.lastref_path(digest)?;
        // Open with create+write to ensure the sidecar exists; an empty file
        // is fine — only its mtime matters.
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&sidecar)?;
        let _ = fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600));
        // Set both atime and mtime to now via `filetime`-equivalent in std:
        // we don't have the `filetime` crate, so set mtime by calling `utimensat`.
        let now = SystemTime::now();
        let dur = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let times = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: dur.as_secs() as _,
                tv_nsec: dur.subsec_nanos() as _,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: dur.as_secs() as _,
                tv_nsec: dur.subsec_nanos() as _,
            },
        };
        // futimens via the borrowed fd is portable across noatime mounts.
        rustix::fs::futimens(&f, &times).map_err(|e| CacheError::Io(std::io::Error::from(e)))?;
        Ok(())
    }

    /// Read the `.lastref` mtime for a blob, or `None` if missing.
    pub fn lastref_mtime(&self, digest: &str) -> Result<Option<SystemTime>, CacheError> {
        let path = self.lastref_path(digest)?;
        match fs::metadata(&path) {
            Ok(m) => Ok(Some(m.modified()?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Walk the cache and assemble a status snapshot. Holds the read lock
    /// for the duration.
    pub fn status(&self) -> Result<CacheStatus, CacheError> {
        let _r = self
            .inner
            .lock
            .read()
            .map_err(|_| CacheError::LockPoisoned)?;
        let blobs_dir = self.inner.root.join("blobs").join("sha256");
        let mut entries: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut oldest: Option<SystemTime> = None;
        if blobs_dir.exists() {
            for child in fs::read_dir(&blobs_dir)? {
                let child = child?;
                let name = child.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(LASTREF_SUFFIX) || name_str.ends_with(TEMP_SUFFIX) {
                    continue;
                }
                let meta = match child.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if !meta.is_file() {
                    continue;
                }
                entries += 1;
                total_bytes += meta.len();
                let digest = format!("sha256:{name_str}");
                if let Ok(Some(t)) = self.lastref_mtime(&digest) {
                    match oldest {
                        Some(prev) if t < prev => oldest = Some(t),
                        None => oldest = Some(t),
                        _ => {}
                    }
                }
            }
        }
        let oldest_entry_age_secs = oldest.and_then(|t| {
            SystemTime::now()
                .duration_since(t)
                .ok()
                .map(|d| d.as_secs())
        });
        Ok(CacheStatus {
            entries,
            total_bytes,
            oldest_entry_age_secs,
        })
    }

    /// Acquire the GC write lock for the duration of the closure. Pulls
    /// briefly contend on the read side only when reserving — they do not
    /// hold the lock for the duration of the download itself, only the
    /// reservation. The reservation map is what protects in-flight work.
    pub(crate) fn with_gc_lock<F, T>(&self, f: F) -> Result<T, CacheError>
    where
        F: FnOnce() -> Result<T, CacheError>,
    {
        let _w = self
            .inner
            .lock
            .write()
            .map_err(|_| CacheError::LockPoisoned)?;
        f()
    }

    /// List `(digest, blob_path, lastref_mtime, size_bytes)` for every cached
    /// blob. Returns a vector so the caller (GC) can iterate without holding
    /// any lock; the GC should be calling this from inside `with_gc_lock`.
    pub(crate) fn list_blobs(&self) -> Result<Vec<BlobListEntry>, CacheError> {
        let blobs_dir = self.inner.root.join("blobs").join("sha256");
        let mut out = Vec::new();
        if !blobs_dir.exists() {
            return Ok(out);
        }
        for child in fs::read_dir(&blobs_dir)? {
            let child = child?;
            let name = child.file_name();
            let name_str = name.to_string_lossy().to_string();
            if name_str.ends_with(LASTREF_SUFFIX) || name_str.ends_with(TEMP_SUFFIX) {
                continue;
            }
            let meta = match child.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let digest = format!("sha256:{name_str}");
            let mtime = self.lastref_mtime(&digest).ok().flatten();
            out.push((digest, child.path(), mtime, meta.len()));
        }
        Ok(out)
    }
}

/// SHA-256 of a file, returning the hex-encoded digest (no `sha256:` prefix).
pub(crate) fn hash_file_sha256(path: &Path) -> Result<String, CacheError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Reject any hex segment that looks like a path traversal, has separators,
/// or is not pure ASCII hex. We never path-join unsanitized digests.
fn sanitize_hex(hex: &str) -> Result<(), CacheError> {
    if hex.is_empty() || hex.len() > 128 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CacheError::UnsupportedDigest(format!("sha256:{hex}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_cache() -> (tempfile::TempDir, LayerCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = LayerCache::new(dir.path().to_path_buf(), OciCacheVerifyMode::Size).unwrap();
        (dir, cache)
    }

    fn write_blob(cache: &LayerCache, digest: &str, bytes: &[u8]) -> PathBuf {
        let path = cache.blob_path(digest).unwrap();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        cache.touch_lastref(digest).unwrap();
        path
    }

    #[test]
    fn blob_path_layout_matches_oci_distribution() {
        let (_g, cache) = fresh_cache();
        let p = cache.blob_path("sha256:abcdef1234567890").unwrap();
        assert!(p.ends_with("blobs/sha256/abcdef1234567890"));
    }

    #[test]
    fn rejects_non_sha256_or_unsafe_hex() {
        let (_g, cache) = fresh_cache();
        assert!(matches!(
            cache.blob_path("md5:abcd"),
            Err(CacheError::UnsupportedDigest(_))
        ));
        assert!(matches!(
            cache.blob_path("sha256:../escape"),
            Err(CacheError::UnsupportedDigest(_))
        ));
        assert!(matches!(
            cache.blob_path("sha256:abc/def"),
            Err(CacheError::UnsupportedDigest(_))
        ));
    }

    #[test]
    fn get_returns_path_on_hit_and_touches_sidecar() {
        let (_g, cache) = fresh_cache();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"hello")));
        write_blob(&cache, &digest, b"hello");
        // Backdate sidecar.
        let sidecar = cache.lastref_path(&digest).unwrap();
        let backdate = SystemTime::now() - Duration::from_secs(60 * 60);
        rustix::fs::futimens(
            fs::File::open(&sidecar).unwrap(),
            &rustix::fs::Timestamps {
                last_access: rustix::fs::Timespec {
                    tv_sec: backdate
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as _,
                    tv_nsec: 0,
                },
                last_modification: rustix::fs::Timespec {
                    tv_sec: backdate
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as _,
                    tv_nsec: 0,
                },
            },
        )
        .unwrap();
        let before = cache.lastref_mtime(&digest).unwrap().unwrap();

        let hit = cache.get(&digest, Some(b"hello".len() as u64)).unwrap();
        assert!(hit.is_some(), "expected cache hit");
        let after = cache.lastref_mtime(&digest).unwrap().unwrap();
        assert!(after > before, "lastref must be touched on hit");
    }

    #[test]
    fn size_mode_evicts_corrupt_blob() {
        let (_g, cache) = fresh_cache();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"correct")));
        // Plant a truncated blob (claims to be `"correct"` but is only `"corr"`).
        write_blob(&cache, &digest, b"corr");
        let res = cache.get(&digest, Some(b"correct".len() as u64));
        assert!(
            matches!(res, Err(CacheError::SizeMismatch { .. })),
            "size mode must reject truncated blob"
        );
        assert!(!cache.blob_path(&digest).unwrap().exists());
    }

    #[test]
    fn full_mode_evicts_silently_corrupted_blob() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LayerCache::new(dir.path().to_path_buf(), OciCacheVerifyMode::Full).unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"correct payload")));
        // Plant bytes with the *correct size* but the wrong contents.
        let wrong = b"WRONG_payload__";
        assert_eq!(wrong.len(), "correct payload".len());
        write_blob(&cache, &digest, wrong);
        let res = cache.get(&digest, Some(wrong.len() as u64));
        assert!(matches!(res, Err(CacheError::DigestMismatch { .. })));
        assert!(!cache.blob_path(&digest).unwrap().exists());
    }

    #[test]
    fn finalize_temp_promotes_atomically() {
        let (_g, cache) = fresh_cache();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"abcd")));
        let tmp = cache.temp_path(&digest).unwrap();
        if let Some(parent) = tmp.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&tmp, b"abcd").unwrap();
        let final_path = cache.finalize_temp(&digest, &tmp).unwrap();
        assert!(final_path.exists());
        assert!(!tmp.exists(), "tmp file must be renamed away");
        // Sidecar must exist after install.
        let sidecar = cache.lastref_path(&digest).unwrap();
        assert!(sidecar.exists());
    }

    #[test]
    fn second_pull_returns_same_path_without_temp_file() {
        // First "pull": stage to tmp, finalize. Second "pull": cache.get
        // returns the exact same path; no `.tmp` file should ever appear
        // again.
        let (_g, cache) = fresh_cache();
        let bytes = b"layer-bytes";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        let tmp = cache.temp_path(&digest).unwrap();
        if let Some(parent) = tmp.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&tmp, bytes).unwrap();
        let first = cache.finalize_temp(&digest, &tmp).unwrap();

        let hit = cache
            .get(&digest, Some(bytes.len() as u64))
            .unwrap()
            .unwrap();
        assert_eq!(first, hit, "second pull must return identical path");
        // No tmp file should exist after finalize.
        assert!(!tmp.exists(), "tmp should be renamed away");
    }

    #[test]
    fn reservation_refcount_blocks_zero_check() {
        let (_g, cache) = fresh_cache();
        let digest = "sha256:deadbeef00000000000000000000000000000000000000000000000000000000";
        let r1 = cache.reserve(digest).unwrap();
        assert_eq!(cache.reservation_count(digest), 1);
        let r2 = cache.reserve(digest).unwrap();
        assert_eq!(cache.reservation_count(digest), 2);
        drop(r1);
        assert_eq!(cache.reservation_count(digest), 1);
        drop(r2);
        assert_eq!(cache.reservation_count(digest), 0);
    }
}
