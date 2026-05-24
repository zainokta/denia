use denia::{
    app::{AppState, build_router},
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandOutput, FakeCommandRunner},
    config::AppConfig,
    domain::{
        CredentialKind, DeploymentRequest, ExternalImageSource, GitSource, HealthCheck,
        ResourceLimits, ServiceConfig, ServiceSource,
    },
    metrics::parse_memory_current,
    secrets::SecretRef,
    state::SqliteStore,
    traefik::{RouteSpec, render_file_provider_config},
};
use tower::util::ServiceExt;

#[test]
fn service_config_requires_explicit_internal_port_and_health_check() {
    let config = ServiceConfig::new(
        "api",
        vec!["api.example.test".to_string()],
        ServiceSource::Git(GitSource {
            repo_url: "git@example.com:acme/api.git".to_string(),
            git_ref: "main".to_string(),
            dockerfile_path: "Dockerfile".to_string(),
            context_path: ".".to_string(),
            credential: SecretRef::new("git-api"),
        }),
        8080,
        HealthCheck::new("/health", 10),
        ResourceLimits::default(),
    )
    .expect("valid service config");

    assert_eq!(config.internal_port, 8080);
    assert_eq!(config.health_check.path, "/health");
    assert!(
        ServiceConfig::new(
            "api",
            vec!["api.example.test".to_string()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/api:latest".to_string(),
                credential: None,
            }),
            0,
            HealthCheck::new("/health", 10),
            ResourceLimits::default(),
        )
        .is_err()
    );
}

#[test]
fn sqlite_store_persists_services_credentials_and_deployments() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");

    let credential = store
        .put_credential(
            "registry-main",
            CredentialKind::RegistryBasic,
            SecretRef::new("registry-main"),
        )
        .expect("credential");

    let service = store
        .put_service(
            ServiceConfig::new(
                "web",
                vec!["web.example.test".to_string()],
                ServiceSource::ExternalImage(ExternalImageSource {
                    image: "ghcr.io/acme/web:latest".to_string(),
                    credential: Some(credential.secret_ref.clone()),
                }),
                3000,
                HealthCheck::new("/ready", 5),
                ResourceLimits::default(),
            )
            .expect("valid service"),
        )
        .expect("service");

    let deployment = store
        .create_deployment(DeploymentRequest::external_image(
            service.id,
            "ghcr.io/acme/web:latest",
        ))
        .expect("deployment");

    assert_eq!(store.list_services().expect("services").len(), 1);
    assert_eq!(
        store
            .list_deployments(service.id)
            .expect("deployments")
            .first()
            .expect("deployment")
            .id,
        deployment.id
    );
}

#[test]
fn sqlite_store_persists_local_artifacts_by_digest() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");

    let artifact = store
        .put_artifact(
            ArtifactRecord::new(
                "sha256:abc123",
                ArtifactKind::OciImage,
                ArtifactSource::ExternalRegistry {
                    image: "ghcr.io/acme/web:latest".to_string(),
                },
            )
            .expect("valid artifact"),
        )
        .expect("artifact");

    let artifacts = store.list_artifacts().expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].digest, artifact.digest);
}

#[test]
fn traefik_config_routes_domains_to_loopback_bridge_ports() {
    let yaml = render_file_provider_config(&[RouteSpec {
        service_name: "web".to_string(),
        domains: vec!["web.example.test".to_string()],
        bridge_port: 19080,
    }])
    .expect("traefik yaml");

    assert!(yaml.contains("Host(`web.example.test`)"));
    assert!(yaml.contains("http://127.0.0.1:19080"));
}

#[test]
fn cgroup_memory_parser_reads_current_bytes() {
    assert_eq!(
        parse_memory_current("73400320\n").expect("memory"),
        73_400_320
    );
}

#[tokio::test]
async fn fake_command_runner_records_commands_and_returns_output() {
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "ok\n".to_string(),
        stderr: String::new(),
    }]);

    let output = runner
        .run(
            "sops",
            &["--decrypt", "/var/lib/denia/secrets/git-main.sops.yaml"],
        )
        .await
        .expect("command output");

    assert_eq!(output.stdout, "ok\n");
    assert_eq!(
        runner.commands(),
        vec!["sops --decrypt /var/lib/denia/secrets/git-main.sops.yaml"]
    );
}

#[test]
fn test_config_defines_runtime_paths_and_tool_binaries() {
    let config = AppConfig::for_test("test-token");

    assert_eq!(config.buildkit_binary.to_string_lossy(), "buildctl");
    assert_eq!(config.sops_binary.to_string_lossy(), "sops");
    assert_eq!(config.runtime_dir, config.data_dir.join("runtime"));
    assert_eq!(config.artifact_dir, config.data_dir.join("artifacts"));
}

#[tokio::test]
async fn axum_router_exposes_health_and_requires_admin_token_for_v1() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), store));

    let health = app
        .clone()
        .oneshot(
            http::Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), http::StatusCode::OK);

    let unauthorized = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri("/v1/services")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn axum_router_accepts_service_creation_with_admin_token() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), store));
    let service = ServiceConfig::new(
        "web",
        vec!["web.example.test".to_string()],
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "ghcr.io/acme/web:latest".to_string(),
            credential: None,
        }),
        3000,
        HealthCheck::new("/ready", 5),
        ResourceLimits::default(),
    )
    .expect("service");

    let response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/services")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&service).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn axum_router_accepts_credentials_and_lifecycle_commands_with_admin_token() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), store));

    let credential = serde_json::json!({
        "name": "git-main",
        "kind": "SshDeployKey",
        "secret_ref": "git-main"
    });

    let credential_response = app
        .clone()
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/credentials/git")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&credential).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(credential_response.status(), http::StatusCode::OK);

    let lifecycle_response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/services/018fbcc2-1f1f-7b4a-8c91-4a0fd2b6b00a/start")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(lifecycle_response.status(), http::StatusCode::ACCEPTED);
}
