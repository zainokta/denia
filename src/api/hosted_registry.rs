//! Hosted OCI registry management endpoints (ADR-031).
//!
//! `GET /v1/registry/status` and `POST /v1/registry/gc` are super-admin only.
//! `GET /v1/registry/repositories` is available to any authenticated principal
//! but project-filtered for non-super-admins (you only see repositories in
//! projects you have a role in).

use axum::{Json, Router, extract::State, routing::get, routing::post};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};

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
struct HostedRegistryGcRunView {
    #[serde(flatten)]
    status: HostedRegistryStatusView,
    scanned_blobs: u64,
    deleted_blobs: u64,
    deleted_bytes: u64,
}

#[derive(Debug, Serialize)]
struct TagView {
    tag: String,
    digest: String,
    size: u64,
    updated_at: DateTime<Utc>,
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

fn status_view(state: &AppState) -> Result<HostedRegistryStatusView, ApiError> {
    let gc_status = state.registry_gc.status();
    Ok(HostedRegistryStatusView {
        repositories: state.registry.count_repositories()?,
        blobs: state.registry.count_blobs()?,
        total_bytes: state.registry.total_blob_bytes()?,
        last_gc_at: gc_status.last_gc_at,
        last_gc_deleted_bytes: gc_status.last_gc_deleted_bytes,
    })
}

async fn get_status(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<HostedRegistryStatusView>, ApiError> {
    ensure_super_admin(&principal)?;
    Ok(Json(status_view(&state)?))
}

async fn post_gc(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<HostedRegistryGcRunView>, ApiError> {
    ensure_super_admin(&principal)?;
    let gc = state.registry_gc.clone();
    // Sweep is blocking filesystem I/O; isolate it from the async runtime.
    let report = tokio::task::spawn_blocking(move || gc.sweep_once())
        .await
        .map_err(|e| ApiError::Conflict(format!("gc join error: {e}")))?
        .map_err(|e| ApiError::Conflict(format!("gc error: {e}")))?;
    Ok(Json(HostedRegistryGcRunView {
        status: status_view(&state)?,
        scanned_blobs: report.scanned_blobs,
        deleted_blobs: report.deleted_blobs,
        deleted_bytes: report.deleted_bytes,
    }))
}

async fn list_repositories(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<HostedRepositoryView>>, ApiError> {
    let mut views = Vec::new();
    for repository in state.registry.list_repositories()? {
        // Project-filter for non-super-admins: skip repos in projects the
        // caller has no role in.
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

        // Resolve project + service names. A repo whose project or service was
        // deleted is skipped rather than surfaced with a dangling reference.
        let Some(project) = state.projects.get_project(repository.project_id)? else {
            continue;
        };
        let Some(service) = state.services.get_service(repository.service_id)? else {
            continue;
        };

        let mut tags = Vec::new();
        for tag in state.registry.tags(repository.id)? {
            let size = state
                .registry
                .manifest(repository.id, &tag.manifest_digest)?
                .map(|m| m.size)
                .unwrap_or(0);
            tags.push(TagView {
                tag: tag.tag,
                digest: tag.manifest_digest,
                size,
                updated_at: tag.updated_at,
            });
        }

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
