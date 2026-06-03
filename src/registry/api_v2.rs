//! Hosted OCI registry `/v2` Distribution-shaped routes (ADR-031).
//!
//! Auth reuses the `/v1` bearer middleware; per-route project RBAC is enforced
//! here. Push (PUT/POST/PATCH) requires Operator; pull (GET/HEAD) requires
//! Viewer. Repository names map to `<project>/<service>`. Handler bodies beyond
//! resolution are implemented in Tasks 4 (uploads) and 5 (manifests); they
//! return 501 for now after auth + resolution succeed.

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::Role;
use crate::registry::domain::HostedRepository;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(v2_ping))
        .route(
            "/{project}/{service}/manifests/{reference}",
            get(get_manifest).put(put_manifest),
        )
        .route(
            "/{project}/{service}/blobs/{digest}",
            get(get_blob).head(head_blob),
        )
        .route("/{project}/{service}/blobs/uploads/", post(start_upload))
        .route(
            "/{project}/{service}/blobs/uploads/{upload_id}",
            patch(patch_upload).put(commit_upload),
        )
}

async fn v2_ping(_principal: Principal) -> StatusCode {
    StatusCode::OK
}

fn validate_segment(seg: &str) -> Result<(), ApiError> {
    if seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.contains('/')
        || seg.contains('\\')
        || seg.chars().any(|c| c.is_whitespace())
    {
        return Err(ApiError::BadRequest("invalid repository name".to_string()));
    }
    Ok(())
}

/// Resolve `<project>/<service>` to a hosted repository, enforcing `min` role.
fn resolve_repo(
    state: &AppState,
    principal: &Principal,
    project: &str,
    service: &str,
    min: Role,
) -> Result<HostedRepository, ApiError> {
    validate_segment(project)?;
    validate_segment(service)?;
    let proj = state
        .projects
        .list_projects()?
        .into_iter()
        .find(|p| p.name == project)
        .ok_or_else(|| ApiError::NotFound("unknown project".to_string()))?;
    let svc = state
        .services
        .list_services()?
        .into_iter()
        .find(|s| s.project_id == proj.id && s.name == service)
        .ok_or_else(|| ApiError::NotFound("unknown service".to_string()))?;
    ensure_role(state, principal, proj.id, min)?;
    let repo = state
        .registry
        .ensure_repository(proj.id, svc.id, &format!("{project}/{service}"))?;
    Ok(repo)
}

async fn get_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _reference)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn put_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _reference)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn get_blob(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _digest)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn head_blob(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _digest)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn start_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn patch_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _upload_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}

async fn commit_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, _upload_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let _repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    Ok(StatusCode::NOT_IMPLEMENTED)
}
