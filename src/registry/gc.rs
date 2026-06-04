//! Conservative garbage collection for the hosted OCI registry (ADR-031).
//!
//! Keeps blobs that are referenced by a manifest (manifest digests themselves,
//! plus config/layer digests parsed from manifest bodies), blobs younger than
//! the grace period, and never touches active upload sessions (uploads live in
//! a separate directory and never appear in `RegistryStorage::list_blobs`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};

use crate::registry::repo::HostedRegistryRepo;
use crate::registry::storage::{RegistryStorage, RegistryStorageError};
use crate::repo::error::RepoError;

#[derive(Debug, thiserror::Error)]
pub enum RegistryGcError {
    #[error(transparent)]
    Storage(#[from] RegistryStorageError),
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error("json parse: {0}")]
    Json(#[from] serde_json::Error),
}

/// One-shot sweep report. Returned by `POST /v1/registry/gc` and logged by the
/// periodic loop.
#[derive(Debug, Clone, Default)]
pub struct RegistryGcReport {
    pub scanned_blobs: u64,
    pub deleted_blobs: u64,
    pub deleted_bytes: u64,
    pub kept_referenced: u64,
    pub kept_recent: u64,
    pub kept_uploads: u64,
    pub ran_at: Option<DateTime<Utc>>,
}

/// Periodic status snapshot for the `GET /v1/registry/status` endpoint.
#[derive(Debug, Clone, Default)]
pub struct RegistryGcStatus {
    pub last_gc_at: Option<DateTime<Utc>>,
    pub last_gc_deleted_bytes: u64,
    pub last_gc_deleted_blobs: u64,
}

/// The garbage collector. Cloneable handle (shared status) so the HTTP handler
/// and the background task observe equivalent snapshots.
#[derive(Clone)]
pub struct RegistryGc {
    storage: RegistryStorage,
    repo: HostedRegistryRepo,
    grace: Duration,
    status: Arc<Mutex<RegistryGcStatus>>,
}

impl RegistryGc {
    pub fn new(storage: RegistryStorage, repo: HostedRegistryRepo, grace: Duration) -> Self {
        Self {
            storage,
            repo,
            grace,
            status: Arc::new(Mutex::new(RegistryGcStatus::default())),
        }
    }

    pub fn status(&self) -> RegistryGcStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Read a manifest blob body for the referenced-set walk. A read failure
    /// (missing or unreadable blob) is surfaced as an error so the caller
    /// aborts the sweep — never treats the manifest as referencing nothing.
    fn read_manifest_body(&self, digest: &str) -> Result<Vec<u8>, RegistryGcError> {
        self.storage
            .read_blob(digest)
            .map_err(RegistryGcError::from)
    }

    /// Run one conservative sweep.
    ///
    /// A blob is deleted only when it is (a) not in the referenced set, and
    /// (b) older than the grace period. The referenced set is the union of all
    /// manifest digests, the config/layer digests parsed from every manifest
    /// body, AND — for OCI image indexes / Docker manifest lists — the
    /// per-arch `manifests[]` sub-manifest digests, recursively. Active upload
    /// sessions are never touched.
    ///
    /// If a manifest body recorded in the metadata cannot be read or parsed,
    /// the sweep ABORTS rather than treating that manifest as referencing
    /// nothing: silently dropping its dependencies would orphan live layers on
    /// a transient read error (ADR-031: "deleting a referenced blob breaks
    /// clients").
    pub fn sweep_once(&self) -> Result<RegistryGcReport, RegistryGcError> {
        let started = Utc::now();

        // Build the referenced digest set. Walk every manifest digest known to
        // the metadata; for each, read+parse its body and follow config,
        // layers, and (for indexes) sub-manifests. Sub-manifests discovered
        // this way are themselves walked so their config/layers survive too.
        let mut referenced: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = self.repo.all_manifest_digests()?;
        while let Some(manifest_digest) = queue.pop() {
            if !referenced.insert(manifest_digest.clone()) {
                // Already processed (cycle / shared sub-manifest guard).
                continue;
            }
            let bytes = self.read_manifest_body(&manifest_digest)?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            if let Some(config_digest) = value
                .get("config")
                .and_then(|c| c.get("digest"))
                .and_then(|d| d.as_str())
            {
                referenced.insert(config_digest.to_string());
            }
            if let Some(layers) = value.get("layers").and_then(|l| l.as_array()) {
                for layer in layers {
                    if let Some(d) = layer.get("digest").and_then(|d| d.as_str()) {
                        referenced.insert(d.to_string());
                    }
                }
            }
            // OCI image index / Docker manifest list: each `manifests[]` entry
            // is itself a manifest blob whose config+layers must survive. Queue
            // it so it is walked recursively.
            if let Some(manifests) = value.get("manifests").and_then(|m| m.as_array()) {
                for sub in manifests {
                    if let Some(d) = sub.get("digest").and_then(|d| d.as_str()) {
                        referenced.insert(d.to_string());
                        queue.push(d.to_string());
                    }
                }
            }
        }

        let kept_uploads = self.storage.count_active_uploads()?;
        let cutoff = SystemTime::now()
            .checked_sub(self.grace)
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let mut report = RegistryGcReport {
            kept_uploads,
            ran_at: Some(started),
            ..RegistryGcReport::default()
        };

        for (digest, _path, mtime, size) in self.storage.list_blobs()? {
            report.scanned_blobs += 1;
            if referenced.contains(&digest) {
                report.kept_referenced += 1;
                continue;
            }
            if mtime > cutoff {
                report.kept_recent += 1;
                continue;
            }
            self.storage.delete_blob(&digest)?;
            self.repo.delete_blob_rows(&digest)?;
            report.deleted_blobs += 1;
            report.deleted_bytes += size;
        }

        self.repo.record_gc_run(
            report.scanned_blobs,
            report.deleted_blobs,
            report.deleted_bytes,
        )?;

        if let Ok(mut s) = self.status.lock() {
            s.last_gc_at = Some(started);
            s.last_gc_deleted_bytes = report.deleted_bytes;
            s.last_gc_deleted_blobs = report.deleted_blobs;
        }

        Ok(report)
    }
}

/// Background task: tick on `interval`, run a sweep, log on error. Mirrors the
/// OCI layer cache `gc_run_until_shutdown` pattern.
pub async fn gc_run_until_shutdown(
    gc: RegistryGc,
    interval: Duration,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let gc = gc.clone();
                let res = tokio::task::spawn_blocking(move || gc.sweep_once()).await;
                match res {
                    Ok(Ok(report)) => {
                        eprintln!(
                            "registry-gc sweep complete scanned={} deleted_blobs={} \
                             deleted_bytes={} kept_referenced={} kept_recent={} kept_uploads={}",
                            report.scanned_blobs,
                            report.deleted_blobs,
                            report.deleted_bytes,
                            report.kept_referenced,
                            report.kept_recent,
                            report.kept_uploads,
                        );
                    }
                    Ok(Err(e)) => eprintln!("registry-gc sweep error: {e}"),
                    Err(e) => eprintln!("registry-gc task join error: {e}"),
                }
            }
        }
    }
}
