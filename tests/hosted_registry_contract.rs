use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use denia::app::{AppState, build_router};
use denia::config::AppConfig;
use denia::domain::service::ExternalImageSource;
use denia::domain::{HealthCheck, ServiceConfig, ServiceSource};
use denia::state::SqliteStore;
use sha2::{Digest, Sha256};
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
async fn upload_lifecycle() {
    let app = test_app_with_project_service().await;
    let payload = b"hello hosted registry".to_vec();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&payload)));

    // 1. start upload
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/default/api/blobs/uploads/")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // 2. append bytes
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("authorization", "Bearer test-token")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "patch status {}", resp.status());

    // 3. commit with digest
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("{location}?digest={digest}"))
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 4. fetch the blob
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v2/default/api/blobs/{digest}"))
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
    assert_eq!(body.as_ref(), payload.as_slice());
}

#[tokio::test]
async fn manifest_roundtrip() {
    use sha2::{Digest, Sha256};
    let app = test_app_with_project_service().await;
    let token = "Bearer test-token";
    let media_type = "application/vnd.oci.image.manifest.v1+json";
    let manifest = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000","size":0},"layers":[]}"#.to_vec();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&manifest)));

    // PUT by tag -> 201 + Docker-Content-Digest
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v2/default/api/manifests/latest")
                .header("authorization", token)
                .header("content-type", media_type)
                .body(Body::from(manifest.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        resp.headers()
            .get("docker-content-digest")
            .unwrap()
            .to_str()
            .unwrap(),
        digest
    );

    // GET by tag -> 200, same bytes + media type
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v2/default/api/manifests/latest")
                .header("authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        media_type
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), manifest.as_slice());

    // GET by digest -> 200, same bytes
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v2/default/api/manifests/{digest}"))
                .header("authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), manifest.as_slice());
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

#[tokio::test]
async fn v2_accepts_docker_basic_auth_with_api_token() {
    let app = test_app_with_project_service().await;
    // docker sends Basic base64("user:password"); password is the API token.
    let creds = STANDARD.encode("denia:test-token");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v2/default/api/manifests/latest")
                .header("authorization", format!("Basic {creds}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Auth passed (super-admin); missing manifest => 404, NOT 401.
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v2_unauthenticated_advertises_basic_realm() {
    let app = test_app_with_project_service().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v2/default/api/manifests/latest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get("www-authenticate").is_some());
}
