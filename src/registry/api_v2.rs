//! Hosted OCI registry `/v2` Distribution-shaped routes (ADR-031).
//!
//! Auth reuses the `/v1` bearer middleware; per-route project RBAC is enforced
//! here. Push (PUT/POST/PATCH) requires Operator; pull (GET/HEAD) requires
//! Viewer. Repository names map to `<project>/<service>`. Handler bodies beyond
//! resolution are implemented in Tasks 4 (uploads) and 5 (manifests); they
//! return 501 for now after auth + resolution succeed.

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::Digest as _;

use axum::{
    Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use tokio_stream::StreamExt as _;
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::Role;
use crate::registry::domain::HostedRepository;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(v2_ping))
        .route("/{project}/{service}/tags/list", get(list_tags))
        .route(
            "/{project}/{service}/manifests/{reference}",
            get(get_manifest).put(put_manifest).delete(delete_manifest),
        )
        .route(
            "/{project}/{service}/blobs/{digest}",
            get(get_blob).head(head_blob).delete(delete_blob),
        )
        .route("/{project}/{service}/blobs/uploads/", post(start_upload))
        .route(
            "/{project}/{service}/blobs/uploads/{upload_id}",
            patch(patch_upload).put(commit_upload),
        )
}

/// Auth middleware for `/v2`. Accepts the same bearer tokens as `/v1` AND HTTP
/// Basic auth where the PASSWORD is a Denia API token (so `docker login -u
/// <user> -p <api-token>` works, ECR-style; the username is ignored). On
/// failure it advertises Basic auth via `WWW-Authenticate` so docker clients
/// know to retry with credentials.
pub(crate) async fn registry_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // Docker clients require `Docker-Distribution-Api-Version: registry/2.0` on
    // `/v2/` responses (both 200 and 401) to recognize the endpoint as a
    // Distribution V2 API and run the credential handshake. Without it the push
    // pipeline fires unauthenticated blob probes and never re-applies the Basic
    // credentials, surfacing a bare `unauthorized:`. Set on every response that
    // flows through this middleware (it wraps all `/v2` routes).
    let api_version = HeaderName::from_static("docker-distribution-api-version");
    if let Some(token) = extract_registry_token(request.headers())
        && let Some(principal) = crate::auth::resolve_auth(
            &state.users,
            &state.tokens,
            &token,
            &state.config.admin_token_hash,
            &state.config.admin_token_hmac_key,
        )
    {
        let mut request = request;
        request.extensions_mut().insert(principal);
        let mut resp = next.run(request).await;
        resp.headers_mut()
            .insert(api_version, HeaderValue::from_static("registry/2.0"));
        return resp;
    }
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Denia Registry\""),
    );
    headers.insert(api_version, HeaderValue::from_static("registry/2.0"));
    resp
}

fn extract_registry_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if let Some(bearer) = value.strip_prefix("Bearer ") {
        return Some(bearer.to_string());
    }
    if let Some(basic) = value.strip_prefix("Basic ") {
        let decoded = STANDARD.decode(basic.trim()).ok()?;
        let creds = String::from_utf8(decoded).ok()?;
        let (_user, token) = creds.split_once(':')?;
        return Some(token.to_string());
    }
    None
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
    let repo =
        state
            .registry
            .ensure_repository(proj.id, svc.id, &format!("{project}/{service}"))?;
    Ok(repo)
}

fn storage_err(e: crate::registry::storage::RegistryStorageError) -> ApiError {
    use crate::registry::storage::RegistryStorageError as E;
    use std::io::ErrorKind;
    match e {
        E::InvalidDigest => ApiError::BadRequest("invalid digest".to_string()),
        E::Io(io) => match io.kind() {
            ErrorKind::NotFound => ApiError::NotFound("blob unknown".to_string()),
            // Internal storage faults (permissions, disk full, etc.) are not
            // client conflicts — surface them as 500 so they are logged and not
            // misread as a 409 the client could "retry to resolve".
            _ => ApiError::Conflict("registry storage error".to_string()),
        },
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
    request: Request,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    let (parts, body) = request.into_parts();
    let media_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MANIFEST_MEDIA_TYPE)
        .to_string();

    // Buffer the manifest body with a finite cap. Manifests are small JSON
    // documents; a multi-MiB cap rejects pathological payloads while never
    // approaching the blob-streaming limits.
    let max = state.config.registry_max_manifest_bytes;
    let bytes = collect_body_capped(body, max).await?;

    let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&bytes)));

    // When the client PUTs to a digest reference, the URL digest MUST equal the
    // body digest (Distribution `DIGEST_INVALID`). Otherwise a client believes
    // it stored `sha256:AAA` while we filed `sha256:BBB`.
    if is_digest_reference(&reference) && reference != digest {
        return Err(ApiError::BadRequest(format!(
            "manifest digest mismatch: url={reference} body={digest}"
        )));
    }

    // Referential integrity: every config/layer/sub-manifest digest the body
    // references must already exist as a blob OR manifest in this repository.
    // Distribution rejects manifests with missing referenced content
    // (`MANIFEST_BLOB_UNKNOWN`); accepting them lets a client tag a manifest
    // that 404s mid-pull.
    let referenced = parse_referenced_digests(&bytes)
        .map_err(|e| ApiError::BadRequest(format!("invalid manifest body: {e}")))?;
    for dep in &referenced {
        let exists = state.registry.has_blob(repo.id, dep)?
            || state.registry.manifest(repo.id, dep)?.is_some();
        if !exists {
            return Err(ApiError::BadRequest(format!(
                "manifest references unknown blob: {dep}"
            )));
        }
    }

    let storage = state.registry_storage.clone();
    let dg = digest.clone();
    let store_bytes = bytes.clone();
    let size = blocking(move || storage.put_content(&dg, &store_bytes)).await?;
    state
        .registry
        .put_manifest(repo.id, &digest, &media_type, size)?;
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

/// Collect a request body into memory, rejecting once the cumulative size
/// exceeds `max`. Used for manifest bodies, which are small; blob uploads use
/// the streaming-to-disk path instead.
async fn collect_body_capped(body: axum::body::Body, max: u64) -> Result<Vec<u8>, ApiError> {
    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::BadRequest(format!("body read error: {e}")))?;
        if buf.len() as u64 + chunk.len() as u64 > max {
            return Err(ApiError::PayloadTooLarge(format!(
                "manifest exceeds {max} byte limit"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Stream a request body into the on-disk upload data file for `upload_id`,
/// enforcing a cumulative size cap (`max`) across this and any prior PATCH
/// chunks. Chunks are flushed to disk in batches via `spawn_blocking` so the
/// full body is never held in RAM (ADR-015). Returns the new total length of
/// the upload data file.
async fn stream_to_upload(
    state: &AppState,
    upload_id: Uuid,
    body: axum::body::Body,
    max: u64,
) -> Result<u64, ApiError> {
    // Flush threshold: accumulate up to ~1 MiB before a blocking append so we
    // do not spawn a blocking task per tiny chunk.
    const FLUSH_THRESHOLD: usize = 1024 * 1024;

    // Bytes already persisted from earlier PATCH requests count toward the cap.
    let storage = state.registry_storage.clone();
    let mut total = blocking(move || storage.upload_size(upload_id)).await?;

    let mut stream = body.into_data_stream();
    let mut pending: Vec<u8> = Vec::with_capacity(FLUSH_THRESHOLD);
    let mut last_len = total;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::BadRequest(format!("body read error: {e}")))?;
        total = total.saturating_add(chunk.len() as u64);
        if total > max {
            return Err(ApiError::PayloadTooLarge(format!(
                "blob upload exceeds {max} byte limit"
            )));
        }
        pending.extend_from_slice(&chunk);
        if pending.len() >= FLUSH_THRESHOLD {
            let storage = state.registry_storage.clone();
            let batch = std::mem::take(&mut pending);
            last_len = blocking(move || storage.append_upload(upload_id, &batch)).await?;
        }
    }
    if !pending.is_empty() {
        let storage = state.registry_storage.clone();
        last_len = blocking(move || storage.append_upload(upload_id, &pending)).await?;
    }
    Ok(last_len)
}

/// Parse the start offset from a Distribution-style `Content-Range` value.
/// Accepts the `<start>-<end>` chunk form Docker sends, plus the HTTP
/// `bytes <start>-<end>/<total>` form. Returns `None` if it cannot parse a
/// start offset (the caller then skips range enforcement).
fn parse_content_range_start(value: &str) -> Option<u64> {
    let value = value.trim();
    // Strip an optional `bytes ` unit prefix.
    let spec = value.strip_prefix("bytes ").unwrap_or(value);
    // Take everything before the first `-`, then before any `/`.
    let start = spec.split('-').next()?.trim();
    start.parse::<u64>().ok()
}

/// Parse the config, layer, and image-index sub-manifest digests referenced by
/// a manifest body. Returns an empty set for bodies that carry no references
/// (e.g. an image config blob accidentally PUT as a manifest is rejected by
/// the JSON parse failing or simply has no references). Index media types
/// (`vnd.oci.image.index.v1+json`, Docker manifest list) expose their per-arch
/// sub-manifests under `manifests[].digest`.
fn parse_referenced_digests(bytes: &[u8]) -> Result<Vec<String>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let mut out = Vec::new();
    if let Some(d) = value
        .get("config")
        .and_then(|c| c.get("digest"))
        .and_then(|d| d.as_str())
    {
        out.push(d.to_string());
    }
    if let Some(layers) = value.get("layers").and_then(|l| l.as_array()) {
        for layer in layers {
            if let Some(d) = layer.get("digest").and_then(|d| d.as_str()) {
                out.push(d.to_string());
            }
        }
    }
    if let Some(manifests) = value.get("manifests").and_then(|m| m.as_array()) {
        for m in manifests {
            if let Some(d) = m.get("digest").and_then(|d| d.as_str()) {
                out.push(d.to_string());
            }
        }
    }
    Ok(out)
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
                HeaderValue::from_str(&digest).map_err(|_| ApiError::Conflict("header".into()))?,
            );
            Ok(resp)
        }
        None => Err(ApiError::NotFound("blob unknown".to_string())),
    }
}

/// `GET /v2/<repo>/tags/list` — Distribution tag listing. Returns
/// `{"name":"<project>/<service>","tags":[...]}` ordered by tag name. Enables
/// `docker pull` discovery and dangling-tag pruning.
async fn list_tags(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Viewer)?;
    let tags: Vec<String> = state
        .registry
        .tags(repo.id)?
        .into_iter()
        .map(|t| t.tag)
        .collect();
    let body = serde_json::json!({
        "name": format!("{project}/{service}"),
        "tags": tags,
    });
    let mut resp = (StatusCode::OK, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(resp)
}

/// `DELETE /v2/<repo>/manifests/<reference>` — delete a tag (tag reference) or
/// a manifest + all tags pointing at it (digest reference). The on-disk blob
/// is reclaimed by the GC once unreferenced. Requires Operator.
async fn delete_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, reference)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    if is_digest_reference(&reference) {
        if state.registry.manifest(repo.id, &reference)?.is_none() {
            return Err(ApiError::NotFound("manifest unknown".to_string()));
        }
        state.registry.delete_manifest(repo.id, &reference)?;
    } else {
        if state.registry.tag(repo.id, &reference)?.is_none() {
            return Err(ApiError::NotFound("manifest unknown".to_string()));
        }
        state.registry.delete_tag(repo.id, &reference)?;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `DELETE /v2/<repo>/blobs/<digest>` — drop the per-repo blob row. The shared
/// content-addressed file is reclaimed by the GC once no manifest or blob row
/// references it. Requires Operator.
async fn delete_blob(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, digest)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;
    if !state.registry.has_blob(repo.id, &digest)? {
        return Err(ApiError::NotFound("blob unknown".to_string()));
    }
    state.registry.delete_blob_row(repo.id, &digest)?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn start_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    request: Request,
) -> Result<Response, ApiError> {
    let repo = resolve_repo(&state, &principal, &project, &service, Role::Operator)?;

    // Cross-repo blob mount: `POST .../uploads/?mount=<digest>&from=<repo>`.
    // The on-disk store is global + content-addressed, so mounting is purely a
    // metadata operation: confirm the digest exists somewhere the caller is
    // allowed to read (the `from` repo) and the blob file is present, then add
    // a `hosted_blobs` row for the destination repo. The GC's global reference
    // set keeps the shared blob alive for both repos.
    if let Some(mount_digest) = params.get("mount") {
        if let Some(blob) = mount_source_blob(&state, &principal, params.get("from"), mount_digest)?
        {
            let storage = state.registry_storage.clone();
            let dg = mount_digest.clone();
            let on_disk = blocking(move || storage.blob_size(&dg)).await?;
            if on_disk.is_some() {
                state.registry.put_blob(repo.id, mount_digest, blob)?;
                let location = format!("/v2/{project}/{service}/blobs/{mount_digest}");
                let mut resp = StatusCode::CREATED.into_response();
                let h = resp.headers_mut();
                h.insert(
                    header::LOCATION,
                    HeaderValue::from_str(&location)
                        .map_err(|_| ApiError::Conflict("header".into()))?,
                );
                h.insert(
                    HeaderName::from_static("docker-content-digest"),
                    HeaderValue::from_str(mount_digest)
                        .map_err(|_| ApiError::Conflict("header".into()))?,
                );
                return Ok(resp);
            }
        }
        // Mount miss: fall through to a normal upload session so the client
        // re-pushes the blob (Distribution semantics).
    }

    let upload_id = Uuid::now_v7();
    let storage = state.registry_storage.clone();
    let data_path = blocking(move || storage.create_upload(upload_id)).await?;
    state
        .registry
        .create_upload(upload_id, repo.id, &data_path.to_string_lossy())?;

    // Monolithic single-shot upload: `POST .../uploads/?digest=<digest>` with
    // the blob bytes as the body. Stream the body to disk, verify, and commit
    // in one request (standard client compatibility).
    if let Some(digest) = params.get("digest").cloned() {
        if let Err(e) = stream_to_upload(
            &state,
            upload_id,
            request.into_body(),
            state.config.registry_max_blob_bytes,
        )
        .await
        {
            let s = state.registry_storage.clone();
            let _ = blocking(move || s.delete_upload(upload_id)).await;
            let _ = state.registry.delete_upload(upload_id);
            return Err(e);
        }
        return commit_single_shot(state, repo.id, upload_id, &project, &service, digest).await;
    }

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

/// Resolve the source blob for a cross-repo mount, enforcing that the caller
/// has at least Viewer on the `from` project. Returns the blob's recorded size
/// if the source repo holds it; `None` if not mountable (caller re-pushes).
fn mount_source_blob(
    state: &AppState,
    principal: &Principal,
    from: Option<&String>,
    digest: &str,
) -> Result<Option<u64>, ApiError> {
    let Some(from) = from else {
        return Ok(None);
    };
    let Some((from_project, from_service)) = from.split_once('/') else {
        return Ok(None);
    };
    // Resolve the source repo with a Viewer gate; a missing repo / forbidden
    // access simply means "not mountable" → fall back to a normal upload.
    let src = match resolve_repo(state, principal, from_project, from_service, Role::Viewer) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    state.registry.blob_size(src.id, digest).map_err(Into::into)
}

/// Finish a monolithic upload: hash the streamed data, verify it matches the
/// requested digest, commit durably, and record the blob row.
async fn commit_single_shot(
    state: AppState,
    repo_id: Uuid,
    upload_id: Uuid,
    project: &str,
    service: &str,
    digest: String,
) -> Result<Response, ApiError> {
    let (actual, size) = {
        let storage = state.registry_storage.clone();
        blocking(move || storage.hash_upload(upload_id)).await?
    };
    if actual != digest {
        let s = state.registry_storage.clone();
        let _ = blocking(move || s.delete_upload(upload_id)).await;
        let _ = state.registry.delete_upload(upload_id);
        return Err(ApiError::BadRequest("digest mismatch".to_string()));
    }
    let s = state.registry_storage.clone();
    let dg = digest.clone();
    blocking(move || s.commit_blob(upload_id, &dg)).await?;
    state.registry.put_blob(repo_id, &digest, size)?;
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

async fn patch_upload(
    State(state): State<AppState>,
    principal: Principal,
    Path((project, service, upload_id_str)): Path<(String, String, String)>,
    request: Request,
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

    // If the client sent `Content-Range: <start>-<end>`, enforce that `start`
    // equals the current upload offset. Distribution requires sequential chunk
    // PATCHes; rejecting a mismatched start fails an out-of-order or duplicated
    // chunk early (416) instead of silently corrupting the blob until the
    // commit-time digest check.
    if let Some(range) = request
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_start)
    {
        let storage = state.registry_storage.clone();
        let current = blocking(move || storage.upload_size(upload_id)).await?;
        if range != current {
            return Err(ApiError::BadRequest(format!(
                "out-of-order chunk: Content-Range start {range} != current offset {current}"
            )));
        }
    }

    // Stream the chunk straight to the upload's on-disk data file, enforcing
    // the cumulative size cap as we go. The body is NEVER fully buffered in
    // RAM — this is the ADR-015 bounded-RAM guarantee for the inbound path.
    let new_len = stream_to_upload(
        &state,
        upload_id,
        request.into_body(),
        state.config.registry_max_blob_bytes,
    )
    .await?;
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
    request: Request,
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

    // Stream any trailing body bytes to disk under the same cumulative cap.
    if let Err(e) = stream_to_upload(
        &state,
        upload_id,
        request.into_body(),
        state.config.registry_max_blob_bytes,
    )
    .await
    {
        // Clean up the partial upload on a streaming/size failure.
        let s = state.registry_storage.clone();
        let _ = blocking(move || s.delete_upload(upload_id)).await;
        let _ = state.registry.delete_upload(upload_id);
        return Err(e);
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
