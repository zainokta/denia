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
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&upload_dir).await;
                return Err(ApiError::BadRequest(format!("body read error: {e}")));
            }
        };
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(ApiError::PayloadTooLarge(format!(
                "upload exceeds {} byte limit",
                max_bytes
            )));
        }
        buf.extend_from_slice(&chunk);
    }

    // Write the compressed archive to disk.
    let archive_path = upload_dir.join("context.tar.zst");
    if let Err(e) = tokio::fs::write(&archive_path, &buf).await {
        let _ = tokio::fs::remove_dir_all(&upload_dir).await;
        return Err(ApiError::BadRequest(format!("failed to write archive: {e}")));
    }

    // Extract into <upload_dir>/context/
    let context_dir = upload_dir.join("context");
    if let Err(e) = tokio::fs::create_dir_all(&context_dir).await {
        let _ = tokio::fs::remove_dir_all(&upload_dir).await;
        return Err(ApiError::BadRequest(format!("failed to create context dir: {e}")));
    }

    let limits = ExtractLimits {
        max_uncompressed: state.config.upload_max_uncompressed_bytes,
        max_entries: state.config.upload_max_entries,
    };
    let result = extract_tar_zst(&buf, &context_dir, &limits);
    match result {
        Ok(()) => {}
        Err(ExtractError::Rejected(msg)) => {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(ApiError::BadRequest(format!("archive rejected: {msg}")));
        }
        Err(ExtractError::Io(e)) => {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(ApiError::BadRequest(format!("archive io error: {e}")));
        }
    }

    // Remove the compressed archive, keep only context/
    let _ = tokio::fs::remove_file(&archive_path).await;

    let ttl_secs = i64::try_from(state.config.upload_ttl_secs).unwrap_or(i64::MAX);
    let expires_at = Utc::now()
        + chrono::Duration::try_seconds(ttl_secs)
            .unwrap_or_else(|| chrono::Duration::seconds(3600));

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

    /// Build a tar.zst from a slice of (path, body) pairs.
    fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    /// Build a raw tar archive (uncompressed) with a single regular-file entry
    /// whose name is exactly `name` (no sanitisation). Used to inject paths like
    /// `../escape` that the tar::Builder itself would refuse to write.
    fn raw_tar_with_name(name: &str, body: &[u8]) -> Vec<u8> {
        // POSIX ustar header layout: name[100], mode[8], uid[8], gid[8],
        // size[12], mtime[12], checksum[8], typeflag[1], linkname[100],
        // magic[6], version[2], uname[32], gname[32], devmajor[8],
        // devminor[8], prefix[155], pad[12]  = 512 bytes total.
        let mut header = [0u8; 512];
        // name (bytes 0..100)
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(99);
        header[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        // mode (100..108): octal "0000644\0"
        header[100..107].copy_from_slice(b"0000644");
        header[107] = b'\0';
        // uid / gid: leave as zeros (valid enough for reading)
        // size (124..136): octal of body.len() + NUL  [POSIX ustar bytes 124–135]
        let size_str = format!("{:011o}\0", body.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        // mtime (136..148): "00000000000\0"  [POSIX ustar bytes 136–147]
        header[136..147].copy_from_slice(b"00000000000");
        header[147] = b'\0';
        // checksum (148..156): computed below  [POSIX ustar bytes 148–155]
        // typeflag (156): '0' = regular file
        header[156] = b'0';
        // magic / version (257..265): "ustar  \0"
        header[257..263].copy_from_slice(b"ustar ");
        header[263..265].copy_from_slice(b" \0");
        // Checksum (148..156): sum of all header bytes with chksum field treated as spaces,
        // stored as 6-digit octal + NUL + space.
        header[148..156].copy_from_slice(b"        "); // placeholder (8 spaces per POSIX)
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        let mut out = Vec::with_capacity(512 + body.len().div_ceil(512) * 512 + 1024);
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        // Pad data block to 512-byte boundary
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
        // Two 512-byte zero blocks = end-of-archive
        out.extend([0u8; 1024]);
        out
    }

    fn raw_tar_zst_with_name(name: &str, body: &[u8]) -> Vec<u8> {
        let tar_bytes = raw_tar_with_name(name, body);
        zstd::stream::encode_all(&tar_bytes[..], 0).unwrap()
    }

    // ── Extractor unit tests ────────────────────────────────────────────────

    #[test]
    fn extracts_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("Dockerfile", b"FROM scratch\n"), ("src/main.rs", b"fn main(){}")]);
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        extract_tar_zst(&bytes, dir.path(), &limits).unwrap();
        assert!(dir.path().join("Dockerfile").exists());
        assert!(dir.path().join("src/main.rs").exists());
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = raw_tar_zst_with_name("../escape", b"x");
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_too_many_entries() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<(String, Vec<u8>)> =
            (0..10).map(|i| (format!("f{i}"), vec![0u8])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let bytes = tar_zst(&refs);
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 3,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_oversize_uncompressed() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("big", &vec![7u8; 4096])]);
        let limits = ExtractLimits {
            max_uncompressed: 1024,
            max_entries: 100,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        // Build a tar with a symlink entry manually using the tar 0.4 API.
        let mut tar_buf = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_username("root").unwrap();
        h.set_link_name("/etc/passwd").unwrap();
        h.set_cksum();
        tar_buf
            .append_data(&mut h, "evil_link", std::io::empty())
            .unwrap();
        let tar_bytes = tar_buf.into_inner().unwrap();
        let compressed = zstd::stream::encode_all(&tar_bytes[..], 0).unwrap();
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        assert!(
            extract_tar_zst(&compressed, dir.path(), &limits).is_err(),
            "symlink entry must be rejected"
        );
    }

    // ── Handler integration tests ───────────────────────────────────────────

    #[tokio::test]
    async fn operator_upload_returns_upload_id() {
        let uploads_dir = tempfile::tempdir().unwrap();
        let state = test_state_with_uploads_dir(uploads_dir.path());
        let service = make_service(&state);

        let payload = tar_zst(&[("Dockerfile", b"FROM scratch\n")]);

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

        let payload = tar_zst(&[("Dockerfile", b"FROM scratch\n")]);

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

    #[tokio::test]
    async fn malicious_archive_returns_400() {
        let uploads_dir = tempfile::tempdir().unwrap();
        let state = test_state_with_uploads_dir(uploads_dir.path());
        let service = make_service(&state);

        // Build a tar.zst with a path-traversal entry: ../escape
        let payload = raw_tar_zst_with_name("../escape", b"evil content");

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

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Verify no escape file was written outside any staging dir under uploads_dir
        let escaped = uploads_dir.path().join("escape");
        assert!(
            !escaped.exists(),
            "traversal file must not exist at {}",
            escaped.display()
        );
    }

    // Suppress unused warning for test_state helper kept for future tests
    #[allow(dead_code)]
    fn _use_test_state() {
        let _ = test_state();
    }
}
