//! Conservative GC for the hosted OCI registry (ADR-031, Task 6).
//!
//! These tests construct a `RegistryGc` directly from a tempdir-backed
//! `RegistryStorage` plus an in-memory migrated `HostedRegistryRepo`, then
//! exercise the sweep invariants (referenced blobs kept, unreferenced old
//! blobs deleted, active uploads never touched). A final test drives the
//! `POST /v1/registry/gc` management endpoint through the real router.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use denia::app::{AppState, build_router};
use denia::config::AppConfig;
use denia::registry::gc::RegistryGc;
use denia::registry::repo::HostedRegistryRepo;
use denia::registry::storage::RegistryStorage;
use denia::repo::sqlite::{SqlitePool, run_migrations};
use denia::state::SqliteStore;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Build a `RegistryGc` over a fresh tempdir storage + in-memory repo, plus
/// a seeded repository row to attach manifests/blobs to.
fn gc_harness(
    grace: Duration,
) -> (
    RegistryStorage,
    HostedRegistryRepo,
    RegistryGc,
    Uuid,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = RegistryStorage::new(dir.path().to_path_buf());
    let pool = SqlitePool::open_in_memory().expect("open pool");
    run_migrations(&pool).expect("migrate");
    let repo = HostedRegistryRepo::new(pool);
    let project_id = Uuid::now_v7();
    let service_id = Uuid::now_v7();
    let repository = repo
        .ensure_repository(project_id, service_id, "default/api")
        .expect("ensure repository");
    let gc = RegistryGc::new(storage.clone(), repo.clone(), grace);
    (storage, repo, gc, repository.id, dir)
}

#[test]
fn referenced_blob_is_kept() {
    let (storage, repo, gc, repo_id, _dir) = gc_harness(Duration::from_secs(60 * 60));

    let layer = b"layerdata";
    let layer_digest = digest_of(layer);
    storage
        .put_content(&layer_digest, layer)
        .expect("write layer blob");
    repo.put_blob(repo_id, &layer_digest, layer.len() as u64)
        .expect("record layer blob");

    let manifest_json = format!(
        r#"{{"schemaVersion":2,"config":{{"digest":"sha256:0000"}},"layers":[{{"digest":"{layer_digest}"}}]}}"#
    );
    let manifest_bytes = manifest_json.as_bytes();
    let manifest_digest = digest_of(manifest_bytes);
    storage
        .put_content(&manifest_digest, manifest_bytes)
        .expect("write manifest blob");
    repo.put_manifest(
        repo_id,
        &manifest_digest,
        "application/vnd.oci.image.manifest.v1+json",
        manifest_bytes.len() as u64,
    )
    .expect("record manifest");

    let report = gc.sweep_once().expect("sweep");
    // The layer file is referenced by the manifest body and survives.
    assert!(
        storage.blob_path(&layer_digest).unwrap().exists(),
        "referenced layer blob must survive GC"
    );
    assert!(report.kept_referenced >= 1, "report: {report:?}");
    assert_eq!(report.deleted_blobs, 0, "report: {report:?}");
}

#[test]
fn unreferenced_old_blob_is_deleted() {
    let (storage, _repo, gc, _repo_id, _dir) = gc_harness(Duration::ZERO);

    let orphan = b"orphan-blob-bytes";
    let orphan_digest = digest_of(orphan);
    storage
        .put_content(&orphan_digest, orphan)
        .expect("write orphan blob");
    // No manifest references it, no blob row recorded; grace is zero.

    let report = gc.sweep_once().expect("sweep");
    assert!(
        !storage.blob_path(&orphan_digest).unwrap().exists(),
        "unreferenced old blob must be deleted"
    );
    assert!(report.deleted_blobs >= 1, "report: {report:?}");
    assert!(report.deleted_bytes > 0, "report: {report:?}");
}

#[test]
fn active_upload_is_kept() {
    let (storage, _repo, gc, _repo_id, _dir) = gc_harness(Duration::ZERO);

    let upload_id = Uuid::now_v7();
    storage.create_upload(upload_id).expect("create upload");
    storage
        .append_upload(upload_id, b"partial")
        .expect("append upload");

    let report = gc.sweep_once().expect("sweep");
    assert!(
        storage.upload_data_path(upload_id).exists(),
        "active upload data file must survive GC"
    );
    assert!(report.kept_uploads >= 1, "report: {report:?}");
}

// Builds an app with a seeded service "api" under the real "default" project,
// mirroring tests/hosted_registry_contract.rs.
async fn test_app() -> Router {
    use denia::domain::service::ExternalImageSource;
    use denia::domain::{HealthCheck, ServiceConfig, ServiceSource};

    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let project_id = store.default_project_id().expect("default project id");
    let source = ServiceSource::ExternalImage(ExternalImageSource {
        image: "ghcr.io/acme/api:latest".to_string(),
        credential: None,
        registry_id: None,
        image_ref: None,
    });
    let svc = ServiceConfig::new(
        project_id,
        "api",
        vec![],
        source,
        8080,
        HealthCheck::new("/", 5),
        None,
        vec![],
    )
    .expect("service config");
    let state = AppState::new(AppConfig::for_test("test-token"), &store);
    state.services.put_service(svc).expect("seed service");
    build_router(state)
}

#[tokio::test]
async fn manual_gc_endpoint_returns_counters() {
    let app = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/registry/gc")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert!(
        json.get("scanned_blobs").and_then(|v| v.as_u64()).is_some(),
        "body: {json}"
    );
    assert!(
        json.get("deleted_blobs").and_then(|v| v.as_u64()).is_some(),
        "body: {json}"
    );
    assert!(
        json.get("deleted_bytes").and_then(|v| v.as_u64()).is_some(),
        "body: {json}"
    );
}
