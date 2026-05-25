use std::sync::Arc;

use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    domain::{
        ExternalImageSource, HealthCheck, ResourceLimits, ServiceConfig, ServiceSource,
    },
    domains::{DomainVerifier, DomainVerifyError},
    state::SqliteStore,
    traefik::RouteSpec,
};
use tower::util::ServiceExt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// FakeVerifier
// ---------------------------------------------------------------------------

struct FakeVerifier {
    ok: bool,
}

#[async_trait::async_trait]
impl DomainVerifier for FakeVerifier {
    async fn verify(&self, _hostname: &str, _token: &str) -> Result<(), DomainVerifyError> {
        if self.ok {
            Ok(())
        } else {
            Err(DomainVerifyError::BodyMismatch)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    store
}

fn seed_service(store: &SqliteStore, name: &str) -> ServiceConfig {
    let project_id = store.default_project_id().expect("default project");
    store
        .put_service(
            ServiceConfig::new(
                project_id,
                name,
                vec!["placeholder.example.test".to_string()],
                ServiceSource::ExternalImage(ExternalImageSource {
                    image: "ghcr.io/acme/web:latest".to_string(),
                    credential: None,
                }),
                3000,
                HealthCheck::new("/ready", 5),
                Some(ResourceLimits::default()),
                vec![],
            )
            .expect("service"),
        )
        .expect("stored service")
}

fn build_app_with_verifier(
    store: SqliteStore,
    verifier: Arc<dyn DomainVerifier>,
) -> axum::Router {
    let state = AppState::new(AppConfig::for_test("test-token"), store)
        .with_domain_verifier(verifier);
    build_router(state)
}

fn admin_bearer() -> &'static str {
    "Bearer test-token"
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_domain(
    app: axum::Router,
    service_id: Uuid,
    hostname: &str,
) -> axum::response::Response {
    app.oneshot(
        http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("/v1/services/{service_id}/domains"))
            .header(http::header::AUTHORIZATION, admin_bearer())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                serde_json::to_vec(&serde_json::json!({ "hostname": hostname })).unwrap(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

// ---------------------------------------------------------------------------
// Test 1: POST creates a pending row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_domains_creates_pending_row() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let resp = post_domain(app, service.id, "app.example.com").await;
    assert_eq!(resp.status(), http::StatusCode::CREATED);

    let v = body_json(resp).await;
    assert_eq!(v["status"], "pending");
    assert_eq!(v["hostname"], "app.example.com");
    let token = v["challenge_token"].as_str().expect("challenge_token");
    assert_eq!(token.len(), 64);
}

// ---------------------------------------------------------------------------
// Test 2: Duplicate hostname → 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_domains_rejects_duplicate_hostname() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let r1 = post_domain(app.clone(), service.id, "dup.example.com").await;
    assert_eq!(r1.status(), http::StatusCode::CREATED);

    let r2 = post_domain(app, service.id, "dup.example.com").await;
    assert_eq!(r2.status(), http::StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Test 3: verify marks verified on match
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_verify_marks_verified_on_match() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let create_resp = post_domain(app.clone(), service.id, "ok.example.com").await;
    assert_eq!(create_resp.status(), http::StatusCode::CREATED);
    let created = body_json(create_resp).await;
    let domain_id = created["id"].as_str().expect("id");

    let verify_resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri(format!(
                    "/v1/services/{}/domains/{domain_id}/verify",
                    service.id
                ))
                .header(http::header::AUTHORIZATION, admin_bearer())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(verify_resp.status(), http::StatusCode::OK);
    let v = body_json(verify_resp).await;
    assert_eq!(v["status"], "verified");
    assert!(v["verified_at"].is_string(), "verified_at should be set");
}

// ---------------------------------------------------------------------------
// Test 4: verify marks failed on mismatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_verify_marks_failed_on_mismatch() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: false }));

    let create_resp = post_domain(app.clone(), service.id, "fail.example.com").await;
    let created = body_json(create_resp).await;
    let domain_id = created["id"].as_str().expect("id");

    let verify_resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri(format!(
                    "/v1/services/{}/domains/{domain_id}/verify",
                    service.id
                ))
                .header(http::header::AUTHORIZATION, admin_bearer())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(verify_resp.status(), http::StatusCode::OK);
    let v = body_json(verify_resp).await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["last_error"], "body mismatch");
}

// ---------------------------------------------------------------------------
// Test 5: delete removes the row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_domain_removes_row() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let create_resp = post_domain(app.clone(), service.id, "del.example.com").await;
    assert_eq!(create_resp.status(), http::StatusCode::CREATED);
    let created = body_json(create_resp).await;
    let domain_id = created["id"].as_str().expect("id");

    let del_resp = app
        .clone()
        .oneshot(
            http::Request::builder()
                .method(http::Method::DELETE)
                .uri(format!(
                    "/v1/services/{}/domains/{domain_id}",
                    service.id
                ))
                .header(http::header::AUTHORIZATION, admin_bearer())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_resp.status(), http::StatusCode::NO_CONTENT);

    let list_resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri(format!("/v1/services/{}/domains", service.id))
                .header(http::header::AUTHORIZATION, admin_bearer())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), http::StatusCode::OK);
    let v = body_json(list_resp).await;
    assert_eq!(v.as_array().expect("array").len(), 0);
}

// ---------------------------------------------------------------------------
// Test 6: challenge endpoint returns token body (no auth)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn challenge_endpoint_returns_token_body() {
    let store = make_store();
    let service = seed_service(&store, "web");
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let create_resp = post_domain(app.clone(), service.id, "challenge.example.com").await;
    assert_eq!(create_resp.status(), http::StatusCode::CREATED);
    let created = body_json(create_resp).await;
    let token = created["challenge_token"].as_str().expect("challenge_token");

    let resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri(format!("/.well-known/denia-challenge/{token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);

    let content_type = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/plain"),
        "expected text/plain, got: {content_type}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), token);
}

// ---------------------------------------------------------------------------
// Test 7: challenge endpoint 404 for unknown token
// ---------------------------------------------------------------------------

#[tokio::test]
async fn challenge_endpoint_404_for_unknown_token() {
    let store = make_store();
    let app = build_app_with_verifier(store, Arc::new(FakeVerifier { ok: true }));

    let resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri("/.well-known/denia-challenge/deadbeef")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Test 8: verify re-renders traefik when route exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_re_renders_traefik_when_route_exists() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let tmp_path = tmp.path().to_path_buf();

    let store = make_store();
    let service = seed_service(&store, "web");

    let mut config = AppConfig::for_test("test-token");
    config.traefik_dynamic_config_path = tmp_path.clone();

    let state = AppState::new(config, store.clone()).with_domain_verifier(Arc::new(FakeVerifier { ok: true }));

    // Pre-insert a RouteSpec so rerender_traefik finds a prev entry with a bridge_port.
    {
        let mut routes = state.routes.lock().unwrap();
        routes.insert(
            service.name.clone(),
            RouteSpec {
                route_key: format!("svc-{}", service.id),
                service_name: service.name.clone(),
                domains: vec![],
                bridge_port: 19000,
                tls: false,
            },
        );
    }

    let app = build_router(state);

    // Create a domain
    let create_resp = post_domain(app.clone(), service.id, "app.example.com").await;
    assert_eq!(create_resp.status(), http::StatusCode::CREATED);
    let created = body_json(create_resp).await;
    let domain_id = created["id"].as_str().expect("id");

    // Verify it (FakeVerifier ok=true)
    let verify_resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri(format!(
                    "/v1/services/{}/domains/{domain_id}/verify",
                    service.id
                ))
                .header(http::header::AUTHORIZATION, admin_bearer())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verify_resp.status(), http::StatusCode::OK);

    // Read the rendered traefik config
    let yaml = std::fs::read_to_string(&tmp_path).expect("traefik config written");
    assert!(
        yaml.contains("Host(`app.example.com`)"),
        "expected Host rule for app.example.com in:\n{yaml}"
    );
    assert!(
        yaml.contains("denia-challenge"),
        "expected denia-challenge router in:\n{yaml}"
    );
}
