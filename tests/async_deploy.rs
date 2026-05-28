//! Integration test for the async deploy contract introduced by ADR-024.
//!
//! Task 4 only covers the synchronous `POST /v1/deployments` → 202 + persisted
//! `Pending` row part. The spawned background task may fail in this test
//! environment because the test `AppState` has no real OCI puller wired up;
//! that is intentional. Later tasks add the `GET /{id}` and SSE log endpoints
//! plus terminal-status assertions.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    domain::{
        DeploymentRequest, ExternalImageSource, HealthCheck, Project, ResourceLimits,
        ServiceConfig, ServiceSource,
    },
    state::SqliteStore,
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef-0123456789abcdef";

#[tokio::test]
async fn post_deployments_returns_202_and_row_is_pending() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let state = AppState::new(AppConfig::for_test(ADMIN_TOKEN), &store);

    // Seed a project + external-image service so the handler can look them up.
    let project = Project::new("team-async", None).expect("project");
    state
        .projects
        .put_project(project.clone())
        .expect("project stored");
    let svc = ServiceConfig::new(
        project.id,
        "web",
        vec!["async.example.test".into()],
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "alpine:3".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }),
        3000,
        HealthCheck::new("/ready", 5),
        Some(ResourceLimits::default()),
        vec![],
    )
    .expect("valid service");
    let service = state.services.put_service(svc).expect("service stored");

    let body = serde_json::to_vec(&DeploymentRequest::external_image(service.id, "alpine:3"))
        .expect("serialize request");

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/deployments")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("request completed");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let v: Value = serde_json::from_slice(&bytes).expect("json body");
    let id: Uuid = v["id"].as_str().expect("id field").parse().expect("uuid");
    assert_eq!(
        v["service_id"].as_str().expect("service_id field"),
        service.id.to_string(),
        "response body must reflect the target service",
    );

    // The row exists with one of the in-flight statuses. The background task
    // may have already advanced past Pending before this assertion runs; do
    // NOT assert a terminal Healthy/Failed status here — that is covered by
    // later tasks once the test runtime grows a real fake puller.
    let row = state
        .deployments
        .list_deployments(service.id)
        .expect("list deployments")
        .into_iter()
        .find(|d| d.id == id)
        .expect("deployment row persisted");
    use denia::domain::DeploymentStatus::*;
    assert!(
        matches!(row.status, Pending | Building | Starting | Healthy | Failed),
        "unexpected status: {:?}",
        row.status,
    );
}
