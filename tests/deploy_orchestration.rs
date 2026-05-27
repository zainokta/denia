use chrono::Utc;
use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    bridge::{BridgeAllocator, BridgeTarget, FakeBridgeManager},
    deploy::{DeploymentCoordinator, DeploymentPlan, DeploymentRepos},
    domain::{
        DeploymentStatus, DomainStatus, ExternalImageSource, HealthCheck, ResourceLimits,
        RuntimeInstanceId, RuntimeStartRequest, ServiceConfig, ServiceDomain, ServiceSource,
    },
    health::FakeHealthChecker,
    repo::sqlite::{SqliteDeploymentRepo, SqliteDomainRepo, SqliteProjectRepo, SqliteRegistryRepo},
    runtime::{FakeRuntime, Runtime},
    state::SqliteStore,
};
use uuid::Uuid;

fn build_repos(store: &SqliteStore) -> DeploymentRepos {
    let pool = store.pool();
    DeploymentRepos {
        deployments: SqliteDeploymentRepo::new(pool.clone()),
        projects: SqliteProjectRepo::new(pool.clone()),
        registries: SqliteRegistryRepo::new(pool.clone()),
        domains: SqliteDomainRepo::new(pool),
    }
}

fn seed_verified_domain(store: &SqliteStore, service_id: Uuid, hostname: &str) {
    store
        .put_service_domain(&ServiceDomain {
            id: Uuid::now_v7(),
            service_id,
            hostname: hostname.into(),
            status: DomainStatus::Verified,
            challenge_token: denia::verification::generate_token(),
            verified_at: Some(Utc::now()),
            last_check_at: None,
            last_error: None,
            created_at: Utc::now(),
        })
        .unwrap();
}

#[test]
fn bridge_allocator_assigns_stable_loopback_ports() {
    let mut allocator = BridgeAllocator::new(19000);

    let first = allocator
        .assign("web", "/var/lib/denia/runtime/web/current.sock".into())
        .expect("first assign");
    let second = allocator
        .assign("web", "/var/lib/denia/runtime/web/current.sock".into())
        .expect("second assign");

    assert_eq!(first.port, 19000);
    assert_eq!(second.port, 19000);
    assert_eq!(
        first,
        BridgeTarget {
            service_name: "web".to_string(),
            port: 19000,
            socket_path: "/var/lib/denia/runtime/web/current.sock".into(),
        }
    );
}

#[tokio::test]
async fn fake_runtime_starts_and_stops_service() {
    let runtime = FakeRuntime::default();
    let artifact = ArtifactRecord::new(
        "sha256:abc123",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:latest".to_string(),
        },
    )
    .expect("artifact");

    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "web".to_string(),
            service_id: uuid::Uuid::now_v7(),
            deployment_id: uuid::Uuid::now_v7(),
            artifact,
            internal_port: 3000,
            socket_path: "/var/lib/denia/runtime/web/current.sock".into(),
            cpu_millis: 500,
            memory_bytes: 536870912,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await
        .expect("started");

    assert_eq!(status.service_name, "web");
    assert_eq!(status.state, "running");

    runtime
        .stop(&RuntimeInstanceId {
            service_id: status.service_id,
            service_name: status.service_name.clone(),
            replica_index: 0,
        })
        .await
        .expect("stopped");
    assert_eq!(runtime.stopped_services(), vec!["web"]);
}

#[tokio::test]
async fn coordinator_promotes_only_after_health_check_passes() {
    let store = SqliteStore::open_in_memory().expect("sqlite");
    store.migrate().expect("migrate");
    let runtime = FakeRuntime::default();
    let health = FakeHealthChecker::healthy();
    let coordinator = DeploymentCoordinator::new(build_repos(&store), runtime, health);

    let project_id = store.default_project_id().expect("default project");
    let service = store
        .put_service(
            ServiceConfig::new(
                project_id,
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
            .expect("service"),
        )
        .expect("stored service");

    let artifact = ArtifactRecord::new(
        "sha256:abc123",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:latest".to_string(),
        },
    )
    .expect("artifact");

    let deployment = coordinator
        .deploy(DeploymentPlan { service, artifact })
        .await
        .expect("deployment");

    assert_eq!(deployment.status, DeploymentStatus::Healthy);
    assert_eq!(
        store
            .promoted_deployment(deployment.service_id)
            .expect("promoted"),
        Some(deployment.id)
    );
    assert_eq!(
        store
            .list_deployments(deployment.service_id)
            .expect("deployments")[0]
            .status,
        DeploymentStatus::Healthy
    );
    assert_eq!(
        store
            .get_deployment_artifact(deployment.id)
            .expect("artifact lookup")
            .expect("artifact linked")
            .digest,
        "sha256:abc123"
    );
}

#[tokio::test]
async fn coordinator_writes_traefik_config_on_promotion() {
    let store = SqliteStore::open_in_memory().expect("sqlite");
    store.migrate().expect("migrate");
    let runtime = FakeRuntime::default();
    let health = FakeHealthChecker::healthy();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("denia.yml");
    let coordinator = DeploymentCoordinator::new_with_routing(
        build_repos(&store),
        runtime,
        health,
        BridgeAllocator::new(19000),
        std::sync::Arc::new(FakeBridgeManager::default()),
        config_path.clone(),
    );

    let project_id = store.default_project_id().expect("default project");
    let service = store
        .put_service(
            ServiceConfig::new(
                project_id,
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
            .expect("service"),
        )
        .expect("stored service");
    let artifact = ArtifactRecord::new(
        "sha256:abc123",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:latest".to_string(),
        },
    )
    .expect("artifact");

    seed_verified_domain(&store, service.id, "web.example.test");

    coordinator
        .deploy(DeploymentPlan { service, artifact })
        .await
        .expect("deployment");

    let content = std::fs::read_to_string(config_path).expect("read config");
    assert!(content.contains("Host(`web.example.test`)"));
    assert!(content.contains("http://127.0.0.1:19000"));
    assert!(content.contains("svc-"));
}
