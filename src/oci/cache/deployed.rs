//! `DeployedDigestSource` backed by the existing SQLite repos plus the
//! per-bundle `layers.json` sidecar written by
//! [`crate::artifacts::acquirer::ArtifactAcquirer::write_bundle`].
//!
//! Per ADR-021, this answers "which layer digests are referenced by some
//! currently-deployed service right now?" without introducing a new table —
//! the artifact_dir already stores one bundle per (image) digest, and each
//! bundle has a sidecar listing its layer digests.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::oci::cache::error::CacheError;
use crate::oci::cache::gc::DeployedDigestSource;
use crate::repo::sqlite::{SqliteDeploymentRepo, SqliteServiceRepo};

/// Reads the union of layer digests across every promoted deployment.
///
/// The mapping path is: services -> promoted deployment -> artifact ->
/// bundle_dir -> layers.json. Bundles without a layers.json sidecar (e.g.
/// pre-ADR-021 bundles) are ignored — those layers won't be tracked but
/// the retention + reservation guards still apply.
pub struct SqliteDeployedDigests {
    pub services: SqliteServiceRepo,
    pub deployments: SqliteDeploymentRepo,
    pub artifact_dir: PathBuf,
}

impl SqliteDeployedDigests {
    pub fn new(
        services: SqliteServiceRepo,
        deployments: SqliteDeploymentRepo,
        artifact_dir: PathBuf,
    ) -> Self {
        Self {
            services,
            deployments,
            artifact_dir,
        }
    }
}

impl DeployedDigestSource for SqliteDeployedDigests {
    fn snapshot(&self) -> Result<BTreeSet<String>, CacheError> {
        let mut acc = BTreeSet::new();
        let services = self
            .services
            .list_services()
            .map_err(|e| CacheError::Io(std::io::Error::other(e.to_string())))?;
        for svc in services {
            let promoted = self
                .deployments
                .promoted_deployment(svc.id)
                .map_err(|e| CacheError::Io(std::io::Error::other(e.to_string())))?;
            let Some(deployment_id) = promoted else {
                continue;
            };
            let Some(artifact) = self
                .deployments
                .get_deployment_artifact(deployment_id)
                .map_err(|e| CacheError::Io(std::io::Error::other(e.to_string())))?
            else {
                continue;
            };
            let bundle = self.artifact_dir.join(safe_artifact_name(&artifact.digest));
            let sidecar = bundle.join("layers.json");
            let bytes = match std::fs::read(&sidecar) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(CacheError::Io(e)),
            };
            let list: Vec<String> = serde_json::from_slice(&bytes)
                .map_err(|e| CacheError::Io(std::io::Error::other(e.to_string())))?;
            for d in list {
                acc.insert(d);
            }
        }
        Ok(acc)
    }
}

/// Mirror of `acquirer::safe_artifact_name`. Kept local so the cache module
/// does not need to depend on `crate::artifacts`.
fn safe_artifact_name(digest: &str) -> String {
    digest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
