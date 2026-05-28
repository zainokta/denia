use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};

use crate::oci::cache::error::CacheError;
use crate::oci::cache::store::LayerCache;

/// Pluggable source of "currently-deployed" blob digests. The deploy
/// coordinator answers from SQLite (promoted deployments → artifact digest →
/// the per-layer digests that artifact produced); tests fake it.
///
/// The trait is intentionally tiny: the GC only asks "is this layer digest
/// referenced by a deployed service right now?" by consulting a snapshot
/// taken at the start of each sweep.
pub trait DeployedDigestSource: Send + Sync {
    fn snapshot(&self) -> Result<BTreeSet<String>, CacheError>;
}

/// Default implementation that always returns an empty set. Wiring will
/// replace this with a `SqliteDeployedDigests` once the artifact->layer
/// mapping is persisted; for now the in-flight reservation guard plus the
/// 7-day default retention keep the cache safe.
pub struct EmptyDeployedDigests;

impl DeployedDigestSource for EmptyDeployedDigests {
    fn snapshot(&self) -> Result<BTreeSet<String>, CacheError> {
        Ok(BTreeSet::new())
    }
}

/// One-shot sweep report. Used by both the periodic loop (logged) and the
/// `POST /v1/oci/cache/gc` endpoint (returned).
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub deleted_entries: u64,
    pub deleted_bytes: u64,
    pub scanned_entries: u64,
    pub kept_in_use_entries: u64,
    pub kept_recent_entries: u64,
    pub ran_at: Option<DateTime<Utc>>,
}

/// Periodic status snapshot for the observability endpoint.
#[derive(Debug, Clone, Default)]
pub struct GcStatus {
    pub last_gc_at: Option<DateTime<Utc>>,
    pub last_gc_deleted_bytes: u64,
    pub last_gc_deleted_entries: u64,
}

/// The garbage collector itself. Cloneable handle (the lock + status are
/// shared) so the HTTP handler and the background task hold equivalent
/// snapshots.
#[derive(Clone)]
pub struct LayerCacheGc {
    cache: LayerCache,
    retention: Duration,
    deployed_source: Arc<dyn DeployedDigestSource>,
    status: Arc<Mutex<GcStatus>>,
    allowed_prefixes: Vec<PathBuf>,
}

impl LayerCacheGc {
    pub fn new(
        cache: LayerCache,
        retention: Duration,
        deployed_source: Arc<dyn DeployedDigestSource>,
        allowed_prefixes: Vec<PathBuf>,
    ) -> Self {
        Self {
            cache,
            retention,
            deployed_source,
            status: Arc::new(Mutex::new(GcStatus::default())),
            allowed_prefixes,
        }
    }

    pub fn cache(&self) -> &LayerCache {
        &self.cache
    }

    pub fn status(&self) -> GcStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Run one sweep and return what was deleted.
    ///
    /// Locking strategy (per ADR-021):
    /// 1. Take the cache's coarse write lock for the duration of the walk.
    ///    This blocks new pull-side reservations briefly but does NOT block
    ///    in-flight downloads (they hold reservations, not the lock).
    /// 2. For every blob: refuse to delete if either (a) the deployed-digest
    ///    snapshot contains it, or (b) the reservation map's count is
    ///    non-zero, or (c) its `.lastref` is newer than `now - retention`.
    /// 3. Otherwise delete the blob and its sidecar.
    /// 4. Update the in-memory `GcStatus`.
    pub fn sweep_once(&self) -> Result<GcReport, CacheError> {
        self.assert_root_under_allowed_prefix()?;

        let deployed = self.deployed_source.snapshot()?;
        let now = SystemTime::now();
        let cutoff = now
            .checked_sub(self.retention)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let started = Utc::now();

        let report = self.cache.with_gc_lock(|| {
            let mut report = GcReport {
                ran_at: Some(started),
                ..GcReport::default()
            };
            for (digest, blob_path, lastref, size) in self.cache.list_blobs()? {
                report.scanned_entries += 1;
                if deployed.contains(&digest) {
                    report.kept_in_use_entries += 1;
                    continue;
                }
                if self.cache.reservation_count(&digest) > 0 {
                    report.kept_in_use_entries += 1;
                    continue;
                }
                let is_old = match lastref {
                    Some(t) => t < cutoff,
                    // No sidecar means we never observed a reference — treat
                    // as expired (the put path always writes one, so this is
                    // either a manual placement or a partially-completed put
                    // whose sidecar got rolled back).
                    None => true,
                };
                if !is_old {
                    report.kept_recent_entries += 1;
                    continue;
                }
                // Re-stat the blob immediately before delete — refuse to
                // delete a path that disappeared between list and delete.
                let cur = std::fs::metadata(&blob_path);
                if cur.is_err() {
                    continue;
                }
                let age_secs = lastref
                    .and_then(|t| now.duration_since(t).ok().map(|d| d.as_secs()))
                    .unwrap_or(0);
                eprintln!(
                    "oci-cache-gc delete digest={digest} size_bytes={size} \
                     last_ref_age_secs={age_secs}"
                );
                let _ = std::fs::remove_file(&blob_path);
                if let Ok(sidecar) = self.cache.lastref_path(&digest) {
                    let _ = std::fs::remove_file(sidecar);
                }
                report.deleted_entries += 1;
                report.deleted_bytes += size;
            }
            Ok(report)
        })?;

        if let Ok(mut s) = self.status.lock() {
            s.last_gc_at = report.ran_at;
            s.last_gc_deleted_bytes = report.deleted_bytes;
            s.last_gc_deleted_entries = report.deleted_entries;
        }
        Ok(report)
    }

    /// Safety rail: refuse to operate if the cache root is not under one of
    /// the configured allowed prefixes (typically `DENIA_DATA_DIR` or the
    /// explicit `DENIA_OCI_CACHE_DIR`). Defends against config corruption
    /// pointing the cache at `/` or `/var` etc.
    fn assert_root_under_allowed_prefix(&self) -> Result<(), CacheError> {
        let root = self.cache.root();
        let canon_root = canonicalize_loose(root);
        for prefix in &self.allowed_prefixes {
            let canon_prefix = canonicalize_loose(prefix);
            if canon_root.starts_with(&canon_prefix) {
                return Ok(());
            }
        }
        Err(CacheError::UnsafeCacheRoot(canon_root))
    }
}

fn canonicalize_loose(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Background task: tick on `interval`, run a sweep, log on error. Mirrors
/// the `run_until_shutdown` pattern from `src/scheduler.rs` and
/// `src/main.rs`'s ACME renewal loop.
pub async fn gc_run_until_shutdown(
    gc: LayerCacheGc,
    interval: Duration,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    // Skip the immediate tick — we don't want to GC the moment the process
    // starts (might block a pending pull's reservation registration in tests).
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let gc = gc.clone();
                // Filesystem walk is blocking; isolate it.
                let res = tokio::task::spawn_blocking(move || gc.sweep_once()).await;
                match res {
                    Ok(Ok(report)) => {
                        eprintln!(
                            "oci-cache-gc sweep complete deleted_entries={} \
                             deleted_bytes={} scanned={} kept_in_use={} kept_recent={}",
                            report.deleted_entries,
                            report.deleted_bytes,
                            report.scanned_entries,
                            report.kept_in_use_entries,
                            report.kept_recent_entries,
                        );
                    }
                    Ok(Err(e)) => eprintln!("oci-cache-gc sweep error: {e}"),
                    Err(e) => eprintln!("oci-cache-gc task join error: {e}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OciCacheVerifyMode;
    use std::fs;
    use std::sync::Arc;

    struct FixedDigests(BTreeSet<String>);
    impl DeployedDigestSource for FixedDigests {
        fn snapshot(&self) -> Result<BTreeSet<String>, CacheError> {
            Ok(self.0.clone())
        }
    }

    fn fresh_cache_at(root: &Path) -> LayerCache {
        LayerCache::new(root.to_path_buf(), OciCacheVerifyMode::None).unwrap()
    }

    fn plant_blob(cache: &LayerCache, digest: &str, bytes: &[u8], age: Duration) {
        let p = cache.blob_path(digest).unwrap();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, bytes).unwrap();
        cache.touch_lastref(digest).unwrap();
        // Backdate the sidecar to simulate an old entry.
        let when = SystemTime::now() - age;
        let dur = when
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let sidecar = cache.lastref_path(digest).unwrap();
        let f = fs::File::options().write(true).open(&sidecar).unwrap();
        rustix::fs::futimens(
            &f,
            &rustix::fs::Timestamps {
                last_access: rustix::fs::Timespec {
                    tv_sec: dur.as_secs() as _,
                    tv_nsec: 0,
                },
                last_modification: rustix::fs::Timespec {
                    tv_sec: dur.as_secs() as _,
                    tv_nsec: 0,
                },
            },
        )
        .unwrap();
    }

    #[test]
    fn deletes_expired_unreferenced_blob() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        plant_blob(&cache, digest, b"x", Duration::from_secs(60 * 60));
        let gc = LayerCacheGc::new(
            cache.clone(),
            Duration::from_secs(60),
            Arc::new(EmptyDeployedDigests),
            vec![dir.path().to_path_buf()],
        );
        let report = gc.sweep_once().unwrap();
        assert_eq!(report.deleted_entries, 1);
        assert!(!cache.blob_path(digest).unwrap().exists());
        assert!(!cache.lastref_path(digest).unwrap().exists());
    }

    #[test]
    fn keeps_recently_used_blob() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let digest = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        plant_blob(&cache, digest, b"x", Duration::from_secs(1));
        let gc = LayerCacheGc::new(
            cache.clone(),
            Duration::from_secs(60 * 60),
            Arc::new(EmptyDeployedDigests),
            vec![dir.path().to_path_buf()],
        );
        let report = gc.sweep_once().unwrap();
        assert_eq!(report.deleted_entries, 0);
        assert_eq!(report.kept_recent_entries, 1);
        assert!(cache.blob_path(digest).unwrap().exists());
    }

    #[test]
    fn refuses_to_delete_blob_referenced_by_deployed_service() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let digest = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
        plant_blob(&cache, digest, b"x", Duration::from_secs(60 * 60));
        let mut deployed = BTreeSet::new();
        deployed.insert(digest.to_string());
        let gc = LayerCacheGc::new(
            cache.clone(),
            Duration::from_secs(60),
            Arc::new(FixedDigests(deployed)),
            vec![dir.path().to_path_buf()],
        );
        let report = gc.sweep_once().unwrap();
        assert_eq!(report.deleted_entries, 0);
        assert_eq!(report.kept_in_use_entries, 1);
        assert!(cache.blob_path(digest).unwrap().exists());
    }

    #[test]
    fn refuses_to_delete_blob_with_live_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let digest = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
        plant_blob(&cache, digest, b"x", Duration::from_secs(60 * 60));
        let _reservation = cache.reserve(digest).unwrap();
        let gc = LayerCacheGc::new(
            cache.clone(),
            Duration::from_secs(60),
            Arc::new(EmptyDeployedDigests),
            vec![dir.path().to_path_buf()],
        );
        let report = gc.sweep_once().unwrap();
        assert_eq!(report.deleted_entries, 0);
        assert!(cache.blob_path(digest).unwrap().exists());
        // After reservation drops, GC may delete on next sweep.
        drop(_reservation);
        let report = gc.sweep_once().unwrap();
        assert_eq!(report.deleted_entries, 1);
    }

    #[test]
    fn refuses_to_operate_outside_allowed_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let gc = LayerCacheGc::new(
            cache,
            Duration::from_secs(60),
            Arc::new(EmptyDeployedDigests),
            vec![PathBuf::from("/this/path/does/not/contain/the/cache")],
        );
        let err = gc.sweep_once().unwrap_err();
        assert!(matches!(err, CacheError::UnsafeCacheRoot(_)));
    }

    /// Holds-off-until-pull-releases: a sweep that starts while a pull
    /// holds a reservation must not delete the blob; a follow-up sweep
    /// after the pull releases must delete it. This emulates the
    /// "concurrent pull + GC" requirement without spawning a real puller.
    #[test]
    fn concurrent_pull_blocks_then_releases() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fresh_cache_at(dir.path());
        let digest = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
        plant_blob(&cache, digest, b"x", Duration::from_secs(60 * 60));
        let gc = LayerCacheGc::new(
            cache.clone(),
            Duration::from_secs(60),
            Arc::new(EmptyDeployedDigests),
            vec![dir.path().to_path_buf()],
        );

        // Simulated pull starts and reserves.
        let reservation = cache.reserve(digest).unwrap();
        let report_during = gc.sweep_once().unwrap();
        assert_eq!(
            report_during.deleted_entries, 0,
            "GC must hold off during pull"
        );
        // Pull completes, reservation drops.
        reservation.release();
        let report_after = gc.sweep_once().unwrap();
        assert_eq!(
            report_after.deleted_entries, 1,
            "GC must delete after pull releases"
        );
    }
}
