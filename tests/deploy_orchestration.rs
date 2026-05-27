use std::sync::Arc;

use chrono::Utc;
use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    deploy::{DeploymentCoordinator, DeploymentPlan, DeploymentRepos},
    domain::{
        DeploymentStatus, DomainStatus, ExternalImageSource, HealthCheck, ResourceLimits,
        RuntimeInstanceId, RuntimeStartRequest, ServiceConfig, ServiceDomain, ServiceSource,
    },
    health::FakeHealthChecker,
    ingress::pingora::IngressState,
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
async fn coordinator_registers_route_and_replica_on_promotion() {
    // Ingress is now the in-process Pingora route table + replica pool (ADR-020);
    // promotion registers a healthy replica for the workload UDS and upserts the
    // verified host into the route table — no Traefik YAML is written.
    let store = SqliteStore::open_in_memory().expect("sqlite");
    store.migrate().expect("migrate");
    let runtime = FakeRuntime::default();
    let health = FakeHealthChecker::healthy();
    let ingress = Arc::new(IngressState::default());
    let coordinator = DeploymentCoordinator::new_with_routing(
        build_repos(&store),
        runtime,
        health,
        ingress.clone(),
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
        .deploy(DeploymentPlan {
            service: service.clone(),
            artifact,
        })
        .await
        .expect("deployment");

    // The verified host resolves to the service in the live route table.
    let route = ingress.routes();
    let resolved = route.resolve("web.example.test").expect("route present");
    assert_eq!(resolved.service_name, "web");
    assert_eq!(resolved.route_key, format!("svc-{}", service.id));
    assert!(!resolved.tls);

    // The workload's UDS is registered as a healthy replica (keyed by service.id).
    assert_eq!(ingress.healthy_count(&service.id.to_string()).await, 1);
    assert!(
        ingress.next_socket(&service.id.to_string()).await.is_some(),
        "a healthy replica socket must be selectable after promotion"
    );
}
