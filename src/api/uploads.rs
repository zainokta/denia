use std::path::{Component, Path};

use axum::{Json, Router, body::Body, extract::{Path as AxumPath, State}, routing::post};
use chrono::Utc;
use serde::Serialize;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::Role;

pub struct ExtractLimits {
    pub max_uncompressed: u64,
    pub max_entries: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive rejected: {0}")]
    Rejected(String),
}

/// Extract a `tar.zst` into `dest`, accepting only regular files and dirs.
///
/// On error, partially-extracted files may remain in `dest`; cleanup is the caller's responsibility.
pub fn extract_tar_zst(bytes: &[u8], dest: &Path, limits: &ExtractLimits) -> Result<(), ExtractError> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut count: u64 = 0;
    let mut total: u64 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        count += 1;
        if count > limits.max_entries {
            return Err(ExtractError::Rejected("too many entries".into()));
        }
        let etype = entry.header().entry_type();
        if !(etype.is_file() || etype.is_dir()) {
            return Err(ExtractError::Rejected(format!("disallowed entry type: {etype:?}")));
        }
        let path = entry.path()?.into_owned();
        for c in path.components() {
            match c {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(ExtractError::Rejected(format!("unsafe path: {}", path.display()))),
            }
        }
        total = total.saturating_add(entry.header().size()?);
        if total > limits.max_uncompressed {
            return Err(ExtractError::Rejected("uncompressed size cap exceeded".into()));
        }
        if !entry.unpack_in(dest)? {
            return Err(ExtractError::Rejected(format!(
                "entry skipped/escaped: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct UploadResponse {
    upload_id: Uuid,
    expires_at: chrono::DateTime<Utc>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/services/{service_id}/uploads",
            post(upload_handler).layer(axum::extract::DefaultBodyLimit::disable()),
        )
}

async fn upload_handler(
    State(state): State<AppState>,
    principal: Principal,
    AxumPath(service_id): AxumPath<Uuid>,
    body: Body,
) -> Result<Json<UploadResponse>, ApiError> {
    let service = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".to_string()))?;
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let upload_id = Uuid::now_v7();
    let upload_dir = state.config.uploads_dir.join(upload_id.to_string());
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| ApiError::BadRequest(format!("failed to create upload dir: {e}")))?;

    // Stream body into memory, enforcing compressed-body cap.
    let max_bytes = state.config.upload_max_bytes;
    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            let _ = std::fs::remove_dir_all(&upload_dir);
            ApiError::BadRequest(format!("body read error: {e}"))
        })?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            let _ = std::fs::remove_dir_all(&upload_dir);
            return Err(ApiError::PayloadTooLarge(format!(
                "upload exceeds {} byte limit",
                max_bytes
            )));
        }
        buf.extend_from_slice(&chunk);
    }

    // Write the compressed archive to disk.
    let archive_path = upload_dir.join("context.tar.zst");
    tokio::fs::write(&archive_path, &buf)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&upload_dir);
            ApiError::BadRequest(format!("failed to write archive: {e}"))
        })?;

    // Extract into <upload_dir>/context/
    let context_dir = upload_dir.join("context");
    tokio::fs::create_dir_all(&context_dir)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&upload_dir);
            ApiError::BadRequest(format!("failed to create context dir: {e}"))
        })?;

    let limits = ExtractLimits {
        max_uncompressed: state.config.upload_max_uncompressed_bytes,
        max_entries: state.config.upload_max_entries,
    };
    let result = extract_tar_zst(&buf, &context_dir, &limits);
    match result {
        Ok(()) => {}
        Err(ExtractError::Rejected(msg)) => {
            let _ = std::fs::remove_dir_all(&upload_dir);
            return Err(ApiError::BadRequest(format!("archive rejected: {msg}")));
        }
        Err(ExtractError::Io(e)) => {
            let _ = std::fs::remove_dir_all(&upload_dir);
            return Err(ApiError::BadRequest(format!("archive io error: {e}")));
        }
    }

    // Remove the compressed archive, keep only context/
    let _ = tokio::fs::remove_file(&archive_path).await;

    let expires_at = Utc::now()
        + chrono::Duration::seconds(state.config.upload_ttl_secs as i64);

    Ok(Json(UploadResponse { upload_id, expires_at }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use crate::domain::{ExternalImageSource, HealthCheck, Role, ServiceConfig, ServiceSource};

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    fn test_state_with_uploads_dir(dir: &std::path::Path) -> AppState {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.uploads_dir = dir.to_path_buf();
        AppState::builder(config).build()
    }

    fn test_state_with_tiny_cap(dir: &std::path::Path) -> AppState {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.uploads_dir = dir.to_path_buf();
        config.upload_max_bytes = 10; // tiny cap to trigger 413
        AppState::builder(config).build()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn make_service(state: &AppState) -> ServiceConfig {
        let project_id = state.projects.default_project_id().unwrap();
        state
            .services
            .put_service(ServiceConfig::new(
                project_id,
                "web",
                Vec::new(),
                ServiceSource::ExternalImage(ExternalImageSource {
                    image: "busybox".to_string(),
                    credential: None,
                    registry_id: None,
                    image_ref: None,
                }),
                8080,
                HealthCheck::new("/", 5),
                None,
                Vec::new(),
            ).unwrap())
            .unwrap()
    }

    /// Build a small valid tar.zst with a single `Dockerfile` entry.
    fn make_tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, *body).unwrap();
        }
        let tar_bytes = tar.into_inner().unwrap();
        zstd::stream::encode_all(&tar_bytes[..], 0).unwrap()
    }

    #[tokio::test]
    async fn operator_upload_returns_upload_id() {
        let uploads_dir = tempfile::tempdir().unwrap();
        let state = test_state_with_uploads_dir(uploads_dir.path());
        let service = make_service(&state);

        let payload = make_tar_zst(&[("Dockerfile", b"FROM scratch\n")]);

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/uploads", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/octet-stream")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let upload_id = body["upload_id"].as_str().expect("upload_id in response");
        assert!(!upload_id.is_empty());

        // Verify context/Dockerfile exists on disk
        let context_dockerfile = uploads_dir
            .path()
            .join(upload_id)
            .join("context")
            .join("Dockerfile");
        assert!(
            context_dockerfile.exists(),
            "context/Dockerfile should exist at {}",
            context_dockerfile.display()
        );
    }

    #[tokio::test]
    async fn viewer_cannot_upload() {
        let uploads_dir = tempfile::tempdir().unwrap();
        let state = test_state_with_uploads_dir(uploads_dir.path());
        let project_id = state.projects.default_project_id().unwrap();
        let viewer = state.users.create_user("viewer", "hash", false).unwrap();
        state
            .users
            .set_membership(viewer.id, project_id, Role::Viewer)
            .unwrap();
        let viewer_token = state
            .tokens
            .create_api_token(viewer.id, "viewer")
            .unwrap()
            .token;
        let service = make_service(&state);

        let payload = make_tar_zst(&[("Dockerfile", b"FROM scratch\n")]);

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/uploads", service.id))
                    .header("Authorization", format!("Bearer {viewer_token}"))
                    .header("Content-Type", "application/octet-stream")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn oversize_body_returns_413() {
        let uploads_dir = tempfile::tempdir().unwrap();
        let state = test_state_with_tiny_cap(uploads_dir.path());
        let service = make_service(&state);

        // Any payload larger than 10 bytes triggers the cap
        let payload = vec![0u8; 20];

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/uploads", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/octet-stream")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
