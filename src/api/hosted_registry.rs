//! Hosted OCI registry management endpoints for the Denia-managed Zot backend.
//!
//! Denia keeps repository ownership/RBAC metadata in SQLite, while Zot owns the
//! registry's blobs, manifests, tags, dedupe, and garbage collection. These
//! endpoints therefore read live tag/manifest state from loopback Zot instead
//! of the legacy Denia blob store.

use std::path::{Path, PathBuf};

use axum::{Json, Router, extract::State, routing::get, routing::post};
use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, HeaderValue};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};

const ZOT_BASE_URL: &str = "http://127.0.0.1:5000";
const OCI_MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/registry/status", get(get_status))
        .route("/registry/gc", post(post_gc))
        .route("/registry/repositories", get(list_repositories))
}

#[derive(Debug, Serialize)]
struct HostedRegistryStatusView {
    repositories: u64,
    blobs: u64,
    total_bytes: u64,
    last_gc_at: Option<DateTime<Utc>>,
    last_gc_deleted_bytes: u64,
}

#[derive(Debug, Serialize)]
struct TagView {
    tag: String,
    digest: String,
    size: u64,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct HostedRepositoryView {
    project_id: Uuid,
    project_name: String,
    service_id: Uuid,
    service_name: String,
    repository: String,
    tags: Vec<TagView>,
}

#[derive(Debug, Deserialize)]
struct ZotTagList {
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    tags: Vec<String>,
}

async fn get_status(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<HostedRegistryStatusView>, ApiError> {
    ensure_super_admin(&principal)?;
    let repositories = state.registry.count_repositories()?;
    let root = PathBuf::from(crate::cli::common::zot::ZOT_STORAGE_DIR);
    let (blobs, total_bytes) = tokio::task::spawn_blocking(move || zot_storage_usage(&root))
        .await
        .map_err(|e| ApiError::Conflict(format!("registry status join error: {e}")))?
        .map_err(|e| ApiError::Conflict(format!("registry status error: {e}")))?;

    Ok(Json(HostedRegistryStatusView {
        repositories,
        blobs,
        total_bytes,
        // Zot owns GC internally. Its Distribution API does not expose a
        // last-sweep timestamp/reclaimed-byte counter, so the legacy fields are
        // intentionally null/zero rather than reporting stale Denia GC state.
        last_gc_at: None,
        last_gc_deleted_bytes: 0,
    }))
}

async fn post_gc(
    State(_state): State<AppState>,
    principal: Principal,
) -> Result<Json<HostedRegistryStatusView>, ApiError> {
    ensure_super_admin(&principal)?;
    Err(ApiError::Conflict(
        "registry garbage collection is managed automatically by Zot".to_string(),
    ))
}

async fn list_repositories(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<HostedRepositoryView>>, ApiError> {
    let client = reqwest::Client::new();
    let mut views = Vec::new();
    for repository in state.registry.list_repositories()? {
        if !principal.is_super_admin {
            let Some(user_id) = principal.user_id else {
                continue;
            };
            if state
                .users
                .role_for(user_id, repository.project_id)?
                .is_none()
            {
                continue;
            }
        }

        let Some(project) = state.projects.get_project(repository.project_id)? else {
            continue;
        };
        let Some(service) = state.services.get_service(repository.service_id)? else {
            continue;
        };

        let tags = zot_tags(&client, &repository.name).await?;
        views.push(HostedRepositoryView {
            project_id: repository.project_id,
            project_name: project.name,
            service_id: repository.service_id,
            service_name: service.name,
            repository: repository.name,
            tags,
        });
    }
    Ok(Json(views))
}

async fn zot_tags(client: &reqwest::Client, repository: &str) -> Result<Vec<TagView>, ApiError> {
    let url = format!("{ZOT_BASE_URL}/v2/{repository}/tags/list");
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ApiError::Conflict(format!("Zot tags request failed: {e}")))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        return Err(ApiError::Conflict(format!(
            "Zot tags request returned {}",
            response.status()
        )));
    }
    let list = response
        .json::<ZotTagList>()
        .await
        .map_err(|e| ApiError::Conflict(format!("invalid Zot tags response: {e}")))?;

    let mut views = Vec::with_capacity(list.tags.len());
    for tag in list.tags {
        let manifest_url = format!("{ZOT_BASE_URL}/v2/{repository}/manifests/{tag}");
        let response = client
            .head(manifest_url)
            .header(ACCEPT, HeaderValue::from_static(OCI_MANIFEST_ACCEPT))
            .send()
            .await
            .map_err(|e| ApiError::Conflict(format!("Zot manifest request failed: {e}")))?;
        if !response.status().is_success() {
            continue;
        }
        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let size = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        views.push(TagView {
            tag,
            digest,
            size,
            // Distribution V2 does not define a tag-updated timestamp.
            updated_at: None,
        });
    }
    Ok(views)
}

fn zot_storage_usage(root: &Path) -> std::io::Result<(u64, u64)> {
    let blobs = root.join("blobs");
    if !blobs.exists() {
        return Ok((0, 0));
    }
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut stack = vec![blobs];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_zot_storage_reports_zero_usage() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(zot_storage_usage(dir.path()).unwrap(), (0, 0));
    }

    #[test]
    fn zot_storage_usage_counts_blob_files() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("blobs/sha256/aa");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("one"), b"1234").unwrap();
        std::fs::write(blob_dir.join("two"), b"123456").unwrap();
        assert_eq!(zot_storage_usage(dir.path()).unwrap(), (2, 10));
    }
}
