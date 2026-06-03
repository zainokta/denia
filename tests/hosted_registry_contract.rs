use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use denia::app::{AppState, build_router};
use denia::config::AppConfig;
use denia::domain::{HealthCheck, ServiceConfig, ServiceSource};
use denia::domain::service::ExternalImageSource;
use denia::state::SqliteStore;
use tower::ServiceExt;

// Builds an app with a seeded service "api" under the real "default" project.
async fn test_app_with_project_service() -> Router {
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
async fn v2_requires_bearer_auth() {
    let app = test_app_with_project_service().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v2/default/api/manifests/latest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
