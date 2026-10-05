//! Operator-requested prune behind `denia clean` (ADR-041).
//!
//! One pass over every Denia-owned store: unreferenced rootfs bundles, crash
//! leftovers, logs, the OCI layer cache (via its GC), and the BuildKit cache.
//! [`run`] either plans (reports reclaimable bytes) or executes. Each category
//! fails independently so one broken store never blocks the others.

pub mod buildkit;
pub mod disk;

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::DeploymentStatus;

/// Bundles touched more recently than this are never pruned. Covers the gap
/// between `write_bundle` and `set_deployment_artifact`, and one-shot jobs
/// launched from a non-promoted digest, without a lock in the deploy path.
pub const BUNDLE_GRACE: Duration = Duration::from_secs(60 * 60);

/// Serializes executions: two concurrent prunes would race on the same
/// candidates and double-count reclaimed bytes.
static EXECUTE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneCategory {
    RootfsBundles,
    CrashLeftovers,
    Logs,
    OciLayerCache,
    BuildkitCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CategoryReport {
    pub category: PruneCategory,
    pub entries: u64,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    /// `false` for a plan (nothing deleted), `true` after execution.
    pub executed: bool,
    pub total_bytes: u64,
    pub categories: Vec<CategoryReport>,
}

#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    #[error("a prune is already running")]
    AlreadyRunning,
}

/// What must survive the prune, gathered from SQLite and the runtime.
#[derive(Debug, Default)]
struct References {
    /// Bundle directory names (`safe_artifact_name(digest)`) still in use.
    bundle_names: BTreeSet<String>,
    live_services: BTreeSet<Uuid>,
    /// Deployments still Pending/Building/Starting; their logs stay.
    active_deployments: BTreeSet<Uuid>,
    in_flight: bool,
}

/// Plan (`execute == false`) or execute the prune.
pub async fn run(state: &AppState, execute: bool) -> Result<PruneReport, PruneError> {
    let _guard = if execute {
        Some(
            EXECUTE_LOCK
                .try_lock()
                .map_err(|_| PruneError::AlreadyRunning)?,
        )
    } else {
        None
    };

    let running = state.runtime.list_running().await;
    let disk_state = state.clone();
    let mut categories = tokio::task::spawn_blocking(move || {
        let running_deployments = running
            .map(|r| r.into_iter().map(|s| s.deployment_id).collect::<Vec<_>>())
            .map_err(|e| format!("listing running replicas failed: {e}"));
        disk_categories(&disk_state, running_deployments, execute)
    })
    .await
    .unwrap_or_else(|e| {
        [
            PruneCategory::RootfsBundles,
            PruneCategory::CrashLeftovers,
            PruneCategory::Logs,
            PruneCategory::OciLayerCache,
        ]
        .into_iter()
        .map(|category| failed(category, format!("prune task failed: {e}")))
        .collect()
    });

    categories.push(buildkit_category(state, execute).await);

    let report = PruneReport {
        executed: execute,
        total_bytes: categories.iter().map(|c| c.bytes).sum(),
        categories,
    };
    if execute {
        tracing::info!(total_bytes = report.total_bytes, "operator prune complete");
    }
    Ok(report)
}

fn disk_categories(
    state: &AppState,
    running_deployments: Result<Vec<Uuid>, String>,
    execute: bool,
) -> Vec<CategoryReport> {
    let config = &state.config;
    let disk = [
        PruneCategory::RootfsBundles,
        PruneCategory::CrashLeftovers,
        PruneCategory::Logs,
    ];
    let refs = match running_deployments.and_then(|running| references(state, &running)) {
        Ok(refs) => refs,
        // Without a reference set nothing can be proven unused.
        Err(error) => {
            let mut out: Vec<_> = disk.into_iter().map(|c| failed(c, error.clone())).collect();
            out.push(oci_category(state, execute));
            return out;
        }
    };

    let now = SystemTime::now();
    let planned = [
        disk::plan_bundles(&config.artifact_dir, &refs.bundle_names, BUNDLE_GRACE, now),
        disk::plan_leftovers(
            &config.artifact_dir,
            &config.uploads_dir,
            refs.in_flight,
            Duration::from_secs(config.upload_ttl_secs),
            now,
        ),
        disk::plan_logs(
            &config.log_dir,
            &refs.live_services,
            &refs.active_deployments,
        ),
    ];
    let allowed_roots = [
        config.artifact_dir.clone(),
        config.uploads_dir.clone(),
        config.log_dir.clone(),
    ];

    let mut out: Vec<_> = disk
        .into_iter()
        .zip(planned)
        .map(|(category, planned)| match planned {
            Err(error) => failed(category, format!("scan failed: {error}")),
            Ok(candidates) if execute => {
                let applied = disk::apply(&candidates, &allowed_roots);
                CategoryReport {
                    category,
                    entries: applied.entries,
                    bytes: applied.bytes,
                    error: (!applied.errors.is_empty()).then(|| applied.errors.join("; ")),
                }
            }
            Ok(candidates) => CategoryReport {
                category,
                entries: candidates.len() as u64,
                bytes: candidates.iter().map(|c| c.bytes).sum(),
                error: None,
            },
        })
        .collect();
    out.push(oci_category(state, execute));
    out
}

fn references(state: &AppState, running: &[Uuid]) -> Result<References, String> {
    fn db(e: impl std::fmt::Display) -> String {
        format!("reading deployments failed: {e}")
    }
    let mut refs = References::default();
    let link = |refs: &mut References, deployment_id: Uuid| -> Result<(), String> {
        if let Some(artifact) = state
            .deployments
            .get_deployment_artifact(deployment_id)
            .map_err(db)?
        {
            refs.bundle_names
                .insert(crate::artifacts::acquirer::safe_artifact_name(
                    &artifact.digest,
                ));
        }
        Ok(())
    };
    for service in state.services.list_services().map_err(db)? {
        refs.live_services.insert(service.id);
        if let Some(promoted) = state
            .deployments
            .promoted_deployment(service.id)
            .map_err(db)?
        {
            link(&mut refs, promoted)?;
        }
        for deployment in state.deployments.list_deployments(service.id).map_err(db)? {
            if matches!(
                deployment.status,
                DeploymentStatus::Pending | DeploymentStatus::Building | DeploymentStatus::Starting
            ) {
                refs.in_flight = true;
                refs.active_deployments.insert(deployment.id);
                link(&mut refs, deployment.id)?;
            }
        }
    }
    for deployment_id in running {
        link(&mut refs, *deployment_id)?;
    }
    Ok(refs)
}

fn oci_category(state: &AppState, execute: bool) -> CategoryReport {
    let category = PruneCategory::OciLayerCache;
    let Some(gc) = state.oci_cache_gc.as_ref() else {
        return CategoryReport {
            category,
            entries: 0,
            bytes: 0,
            error: None,
        };
    };
    let result = if execute {
        gc.sweep_once()
    } else {
        gc.plan_once()
    };
    match result {
        Ok(report) => CategoryReport {
            category,
            entries: report.deleted_entries,
            bytes: report.deleted_bytes,
            error: None,
        },
        Err(error) => failed(category, format!("oci cache gc failed: {error}")),
    }
}

async fn buildkit_category(state: &AppState, execute: bool) -> CategoryReport {
    let category = PruneCategory::BuildkitCache;
    let buildctl = state.config.buildkit_binary.to_string_lossy().into_owned();
    let runner = state.command_runner.as_ref();
    let result = if execute {
        buildkit::prune(runner, &buildctl).await
    } else {
        buildkit::plan(runner, &buildctl).await
    };
    match result {
        Ok(usage) => CategoryReport {
            category,
            entries: usage.entries,
            bytes: usage.bytes,
            error: None,
        },
        Err(error) => failed(category, error),
    }
}

fn failed(category: PruneCategory, error: String) -> CategoryReport {
    CategoryReport {
        category,
        entries: 0,
        bytes: 0,
        error: Some(error),
    }
}
