use denia::{
    app::{AppState, build_router},
    artifacts::acquirer::{ArtifactAcquireRequest, ArtifactAcquirer},
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandOutput, FakeCommandRunner},
    config::AppConfig,
    domain::{
        CredentialKind, DeploymentRequest, ExternalImageSource, GitSource, HealthCheck,
        ResourceLimits, ServiceConfig, ServiceSource,
    },
    logs::LogStore,
    metrics::{parse_cpu_stat, parse_memory_current},
    secrets::{SecretPayload, SecretRef, SopsSecretStore},
    state::SqliteStore,
    traefik::{RouteSpec, render_file_provider_config},
};
use tower::util::ServiceExt;
use uuid::Uuid;

const DEFAULT_PROJECT_ID: Uuid = Uuid::from_u64_pair(1, 0);

#[test]
fn service_config_requires_explicit_internal_port_and_health_check() {
    let config = ServiceConfig::new(
        DEFAULT_PROJECT_ID,
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
        Some(ResourceLimits::default()),
        vec![],
    )
    .expect("valid service config");

    assert_eq!(config.internal_port, 8080);
    assert_eq!(config.health_check.path, "/health");
    assert!(
        ServiceConfig::new(
            DEFAULT_PROJECT_ID,
            "api",
            vec!["api.example.test".to_string()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/api:latest".to_string(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            0,
            HealthCheck::new("/health", 10),
            Some(ResourceLimits::default()),
            vec![],
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
                DEFAULT_PROJECT_ID,
                "web",
                vec!["web.example.test".to_string()],
                ServiceSource::ExternalImage(ExternalImageSource {
                    image: "ghcr.io/acme/web:latest".to_string(),
                    credential: Some(credential.secret_ref.clone()),
                    registry_id: None,
                    image_ref: None,
                }),
                3000,
                HealthCheck::new("/ready", 5),
                Some(ResourceLimits::default()),
                vec![],
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

#[tokio::test]
async fn artifact_acquirer_builds_git_source_with_buildkit() {
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "sha256:build123\n".to_string(),
        stderr: String::new(),
    }]);
    let config = AppConfig::for_test("test-token");
    let acquirer = ArtifactAcquirer::new(config.clone());

    let artifact = acquirer
        .acquire(
            &runner,
            ArtifactAcquireRequest::Git {
                repo_url: "git@example.com:acme/api.git".to_string(),
                git_ref: "main".to_string(),
                dockerfile_path: "Dockerfile".to_string(),
                context_path: ".".to_string(),
            },
        )
        .await
        .expect("artifact");

    assert_eq!(artifact.digest, "sha256:build123");
    assert!(runner.commands()[0].starts_with("buildctl build"));
}

#[tokio::test]
async fn artifact_acquirer_pulls_external_image() {
    use async_trait::async_trait;
    use denia::oci::{
        LayerBlob, OciError, OciImagePuller, OciRootfsUnpacker, PulledImage,
        config::OciImageConfig as OciCfg, config::OciImageProcessConfig,
    };
    use std::sync::Arc;

    struct FakePuller;
    #[async_trait]
    impl OciImagePuller for FakePuller {
        async fn pull(&self, _image: &str) -> Result<PulledImage, OciError> {
            Ok(PulledImage {
                digest: "sha256:pull123".to_string(),
                config: OciCfg {
                    config: Some(OciImageProcessConfig {
                        entrypoint: Some(vec!["/app".to_string()]),
                        cmd: None,
                        env_vars: None,
                        working_dir: None,
                    }),
                    rootfs: None,
                },
                layers: vec![],
            })
        }
        async fn read_layout(&self, _dir: &std::path::Path) -> Result<PulledImage, OciError> {
            unreachable!()
        }
    }
    struct NoopUnpacker;
    impl OciRootfsUnpacker for NoopUnpacker {
        fn unpack(&self, _layers: &[LayerBlob], rootfs: &std::path::Path) -> Result<(), OciError> {
            std::fs::create_dir_all(rootfs).map_err(OciError::Io)
        }
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let mut config = AppConfig::for_test("test-token");
    config.artifact_dir = tmp.path().to_path_buf();
    let runner = FakeCommandRunner::new(vec![]);
    let acquirer =
        ArtifactAcquirer::with_traits(config, Arc::new(FakePuller), Arc::new(NoopUnpacker));

    let artifact = acquirer
        .acquire_rootfs_bundle_from_image_config(
            &runner,
            ArtifactAcquireRequest::ExternalImage {
                image: "ghcr.io/acme/web:latest".to_string(),
            },
        )
        .await
        .expect("artifact");

    assert_eq!(artifact.digest, "sha256:pull123");
    assert_eq!(artifact.kind, ArtifactKind::RootfsBundle);
}

#[test]
fn traefik_config_routes_domains_to_loopback_bridge_ports() {
    let yaml = render_file_provider_config(
        &[RouteSpec {
            route_key: "svc-web".to_string(),
            service_name: "web".to_string(),
            domains: vec!["web.example.test".to_string()],
            bridge_port: 19080,
            tls: false,
        }],
        &denia::traefik::IngressRenderOptions {
            acme_resolver: "le".to_string(),
            control_domain: None,
            control_tls: false,
            control_backend_addr: "http://127.0.0.1:7180".to_string(),
        },
    )
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

#[test]
fn cpu_stat_parser_reads_usage_usec() {
    let stat = parse_cpu_stat("usage_usec 12345\nuser_usec 100\nsystem_usec 50\n").expect("stat");
    assert_eq!(stat.usage_usec, 12345);
}

#[test]
fn log_store_appends_and_reads_service_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let logs = LogStore::new(dir.path());

    logs.append("web", "first line\n").expect("append");
    logs.append("web", "second line\n").expect("append");

    assert_eq!(
        logs.read_recent("web", 2).expect("lines"),
        vec!["first line".to_string(), "second line".to_string()]
    );
}

#[test]
fn sops_secret_store_resolves_secret_paths_under_data_dir() {
    let store = SopsSecretStore::new("/var/lib/denia");
    let path = store.secret_path(&SecretRef::new("git-main"));

    assert_eq!(
        path.to_string_lossy(),
        "/var/lib/denia/secrets/git-main.sops.yaml"
    );
}

#[test]
fn secret_ref_parse_rejects_path_traversal() {
    assert!(SecretRef::parse("../outside").is_err());
    assert!(SecretRef::parse("/tmp/secret").is_err());
}

#[test]
fn secret_ref_deserialize_rejects_path_traversal() {
    let result = serde_json::from_str::<SecretRef>("\"../outside\"");
    assert!(result.is_err());
}

#[test]
fn secret_payload_serializes_without_exposing_metadata() {
    let payload = SecretPayload::new("OPENSSH_PRIVATE_KEY");
    let json = serde_json::to_string(&payload).expect("json");

    assert_eq!(json, "{\"value\":\"OPENSSH_PRIVATE_KEY\"}");
}

#[tokio::test]
async fn sops_secret_store_decrypts_payload_with_runner() {
    let store = SopsSecretStore::new("/var/lib/denia");
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "{\"value\":\"registry-token\"}".to_string(),
        stderr: String::new(),
    }]);

    let payload = store
        .decrypt(
            &runner,
            std::path::Path::new("sops"),
            &SecretRef::new("registry-main"),
        )
        .await
        .expect("payload");

    assert_eq!(payload.value, "registry-token");
    assert_eq!(
        runner.commands(),
        vec!["sops --decrypt /var/lib/denia/secrets/registry-main.sops.yaml"]
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
    assert_eq!(config.unshare_binary.to_string_lossy(), "unshare");
    assert_eq!(config.socket_proxy_binary.to_string_lossy(), "denia");
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
        DEFAULT_PROJECT_ID,
        "web",
        vec!["web.example.test".to_string()],
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "ghcr.io/acme/web:latest".to_string(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }),
        3000,
        HealthCheck::new("/ready", 5),
        Some(ResourceLimits::default()),
        vec![],
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
    assert_eq!(lifecycle_response.status(), http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deployment_endpoint_rejects_unknown_service() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), store));

    let request =
        DeploymentRequest::external_image(uuid::Uuid::now_v7(), "ghcr.io/acme/web:latest");

    let response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/deployments")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&request).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
}

#[test]
fn migrate_is_idempotent_and_records_version() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
    let v = store.schema_version().unwrap();
    assert!(v >= 2);
}

#[test]
fn migration_seeds_default_project_and_backfills_services() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let default_id = store.default_project_id().unwrap();
    let projects = store.list_projects().unwrap();
    assert!(
        projects
            .iter()
            .any(|p| p.id == default_id && p.name == "default")
    );
}
