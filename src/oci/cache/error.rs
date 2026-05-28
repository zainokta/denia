use std::path::PathBuf;

use thiserror::Error;

/// Typed errors at the layer-cache boundary.
///
/// `OciError::Pull` already covers registry failures; `CacheError` is the
/// disk-side counterpart used by [`LayerCache`] and [`LayerCacheGc`].
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported digest format: {0}")]
    UnsupportedDigest(String),
    #[error("digest mismatch on cached blob {digest}: expected size {expected}, got {actual}")]
    SizeMismatch {
        digest: String,
        expected: u64,
        actual: u64,
    },
    #[error("digest mismatch on cached blob {digest}: expected {expected}, got {actual}")]
    DigestMismatch {
        digest: String,
        expected: String,
        actual: String,
    },
    #[error("cache root {0:?} is not under any allowed prefix; refusing destructive operation")]
    UnsafeCacheRoot(PathBuf),
    #[error("cache lock poisoned")]
    LockPoisoned,
}
