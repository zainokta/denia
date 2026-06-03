//! Hosted OCI registry `/v2` Distribution-shaped routes (ADR-031).
//!
//! Auth reuses the `/v1` bearer middleware; per-route project RBAC is enforced
//! here. Push (PUT/POST/PATCH) requires Operator; pull (GET/HEAD) requires
//! Viewer. Repository names map to `<project>/<service>`. Handler bodies beyond
//! resolution are implemented in Tasks 4 (uploads) and 5 (manifests); they
//! return 501 for now after auth + resolution succeed.

use std::collections::HashMap;

use sha2::Digest as _;

use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{StatusCode, header, HeaderName, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use uuid::Uuid;

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

fn storage_err(e: crate::registry::storage::RegistryStorageError) -> ApiError {
    use crate::registry::storage::RegistryStorageError as E;
    match e {
        E::InvalidDigest => ApiError::BadRequest("invalid digest".to_string()),
        E::Io(_) => ApiError::Conflict("registry storage error".to_string()),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, crate::registry::storage::RegistryStorageError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Conflict(format!("join error: {e}")))?
        .map_err(storage_err)
}

fn is_digest_reference(reference: &str) -> bool {
    reference.starts_with("sha256:")
}

const DEFAULT_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

async fn get_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, reference)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    let digest = if is_digest_reference(&reference) {
        reference.clone()
    } else {
        state
            .registry
            .tag(repo.id, &reference)?
            .ok_or_else(|| ApiError::NotFound("manifest unknown".to_string()))?
    };
    let manifest = state
        .registry
        .manifest(repo.id, &digest)?
        .ok_or_else(|| ApiError::NotFound("manifest unknown".to_string()))?;
    let storage = state.registry_storage.clone();
    let dg = digest.clone();
    let bytes = blocking(move || storage.read_blob(&dg)).await?;
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&manifest.media_type)
            .map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        HeaderName::from_static("docker-content-digest"),
        HeaderValue::from_str(&digest).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    Ok(resp)
}

async fn put_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, reference)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MANIFEST_MEDIA_TYPE)
        .to_string();
    let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&body)));
    let storage = state.registry_storage.clone();
    let dg = digest.clone();
    let bytes = body.to_vec();
    let size = blocking(move || storage.put_content(&dg, &bytes)).await?;
    state.registry.put_manifest(repo.id, &digest, &media_type, size)?;
    if !is_digest_reference(&reference) {
        state.registry.put_tag(repo.id, &reference, &digest)?;
    }
    let location = format!("/v2/{project}/{service}/manifests/{digest}");
    let mut resp = StatusCode::CREATED.into_response();
    let h = resp.headers_mut();
    h.insert(
        HeaderName::from_static("docker-content-digest"),
        HeaderValue::from_str(&digest).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    Ok(resp)
}

async fn get_blob(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, digest)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    if !state.registry.has_blob(repo.id, &digest)? {
        return Err(ApiError::NotFound("blob unknown".to_string()));
    }
    let storage = state.registry_storage.clone();
    let dg = digest.clone();
    let bytes = blocking(move || storage.read_blob(&dg)).await?;
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        HeaderName::from_static("docker-content-digest"),
        HeaderValue::from_str(&digest).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    Ok(resp)
}

async fn head_blob(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, digest)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    if !state.registry.has_blob(repo.id, &digest)? {
        return Err(ApiError::NotFound("blob unknown".to_string()));
    }
    let storage = state.registry_storage.clone();
    let dg = digest.clone();
    let size_opt = blocking(move || storage.blob_size(&dg)).await?;
    match size_opt {
        Some(n) => {
            let mut resp = StatusCode::OK.into_response();
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&n.to_string())
                    .map_err(|_| ApiError::Conflict("header".into()))?,
            );
            h.insert(
                HeaderName::from_static("docker-content-digest"),
                HeaderValue::from_str(&digest)
                    .map_err(|_| ApiError::Conflict("header".into()))?,
            );
            Ok(resp)
        }
        None => Err(ApiError::NotFound("blob unknown".to_string())),
    }
}

async fn start_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    let upload_id = Uuid::now_v7();
    let storage = state.registry_storage.clone();
    let data_path = blocking(move || storage.create_upload(upload_id)).await?;
    state.registry.create_upload(upload_id, repo.id, &data_path.to_string_lossy())?;
    let location = format!("/v2/{project}/{service}/blobs/uploads/{upload_id}");
    let mut resp = StatusCode::ACCEPTED.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        HeaderName::from_static("docker-upload-uuid"),
        HeaderValue::from_str(&upload_id.to_string())
            .map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(header::RANGE, HeaderValue::from_static("0-0"));
    Ok(resp)
}

async fn patch_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, upload_id_str)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    let upload_id = Uuid::parse_str(&upload_id_str)
        .map_err(|_| ApiError::BadRequest("invalid upload id".to_string()))?;
    let session = state
        .registry
        .upload(upload_id)?
        .ok_or_else(|| ApiError::NotFound("upload unknown".to_string()))?;
    if session.repository_id != repo.id {
        return Err(ApiError::NotFound("upload unknown".to_string()));
    }
    let storage = state.registry_storage.clone();
    let bytes = body.to_vec();
    let new_len = blocking(move || storage.append_upload(upload_id, &bytes)).await?;
    let location = format!("/v2/{project}/{service}/blobs/uploads/{upload_id}");
    let range = format!("0-{}", new_len.saturating_sub(1));
    let mut resp = StatusCode::ACCEPTED.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        header::RANGE,
        HeaderValue::from_str(&range).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        HeaderName::from_static("docker-upload-uuid"),
        HeaderValue::from_str(&upload_id.to_string())
            .map_err(|_| ApiError::Conflict("header".into()))?,
    );
    Ok(resp)
}

async fn commit_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, upload_id_str)): Path<(String, String, String)>,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    let upload_id = Uuid::parse_str(&upload_id_str)
        .map_err(|_| ApiError::BadRequest("invalid upload id".to_string()))?;
    let session = state
        .registry
        .upload(upload_id)?
        .ok_or_else(|| ApiError::NotFound("upload unknown".to_string()))?;
    if session.repository_id != repo.id {
        return Err(ApiError::NotFound("upload unknown".to_string()));
    }
    let digest = params
        .get("digest")
        .ok_or_else(|| ApiError::BadRequest("missing digest".to_string()))?
        .clone();

    // Append any trailing body bytes
    if !body.is_empty() {
        let storage = state.registry_storage.clone();
        let bytes = body.to_vec();
        blocking(move || storage.append_upload(upload_id, &bytes)).await?;
    }

    // Compute the actual digest
    let storage = state.registry_storage.clone();
    let (actual, size) = blocking(move || storage.hash_upload(upload_id)).await?;

    if actual != digest {
        // Clean up on mismatch
        let s = state.registry_storage.clone();
        let _ = blocking(move || s.delete_upload(upload_id)).await;
        let _ = state.registry.delete_upload(upload_id);
        return Err(ApiError::BadRequest("digest mismatch".to_string()));
    }

    // Commit
    let s = state.registry_storage.clone();
    let dg = digest.clone();
    blocking(move || s.commit_blob(upload_id, &dg)).await?;
    state.registry.put_blob(repo.id, &digest, size)?;
    state.registry.delete_upload(upload_id)?;

    let location = format!("/v2/{project}/{service}/blobs/{digest}");
    let mut resp = StatusCode::CREATED.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    h.insert(
        HeaderName::from_static("docker-content-digest"),
        HeaderValue::from_str(&digest).map_err(|_| ApiError::Conflict("header".into()))?,
    );
    Ok(resp)
}
