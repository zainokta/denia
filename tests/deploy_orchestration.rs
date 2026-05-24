use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    deploy::{DeploymentCoordinator, DeploymentPlan},
    domain::{
        DeploymentStatus, ExternalImageSource, HealthCheck, ResourceLimits, RuntimeStartRequest,
        ServiceConfig, ServiceSource,
    },
    health::FakeHealthChecker,
    runtime::{FakeRuntime, Runtime},
    state::SqliteStore,
};

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
            deployment_id: uuid::Uuid::now_v7(),
            artifact,
            internal_port: 3000,
            socket_path: "/var/lib/denia/runtime/web/current.sock".into(),
            cpu_millis: 500,
            memory_bytes: 536870912,
        })
        .await
        .expect("started");

    assert_eq!(status.service_name, "web");
    assert_eq!(status.state, "running");

    runtime.stop(&status.service_name).await.expect("stopped");
    assert_eq!(runtime.stopped_services(), vec!["web"]);
}

#[tokio::test]
async fn coordinator_promotes_only_after_health_check_passes() {
    let store = SqliteStore::open_in_memory().expect("sqlite");
    store.migrate().expect("migrate");
    let runtime = FakeRuntime::default();
    let health = FakeHealthChecker::healthy();
    let coordinator = DeploymentCoordinator::new(store.clone(), runtime, health);

    let service = store
        .put_service(
            ServiceConfig::new(
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
}
