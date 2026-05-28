//! Persistent content-addressed OCI layer cache.
//!
//! See ADR-021. The cache lives under `<oci_cache_dir>/blobs/<algorithm>/<digest_hex>`
//! with a sibling `<digest_hex>.lastref` mtime sidecar tracking the most recent
//! cache reference. The garbage collector ([`gc::LayerCacheGc`]) deletes blobs
//! whose `.lastref` is older than the configured retention threshold AND that
//! are neither referenced by a deployed service nor held by an in-flight pull
//! reservation.

pub mod deployed;
pub mod error;
pub mod gc;
pub mod store;

pub use deployed::SqliteDeployedDigests;
pub use error::CacheError;
pub use gc::{
    DeployedDigestSource, EmptyDeployedDigests, GcReport, GcStatus, LayerCacheGc,
    gc_run_until_shutdown,
};
pub use store::{CacheReservation, CacheStatus, LayerCache};
