use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::{
    artifacts::acquirer::{ArtifactAcquireError, ArtifactAcquireRequest, ArtifactAcquirer},
    artifacts::{ArtifactRecord, ArtifactSource},
    bridge::{BridgeAllocator, BridgeError, BridgeManager},
    command::CommandRunner,
    domain::ServiceSource,
    domain::{Deployment, DeploymentRequest, DeploymentStatus, RuntimeStartRequest, ServiceConfig},
    health::{HealthChecker, HealthError},
    runtime::{Runtime, RuntimeError},
    state::{SqliteStore, StateError},
    traefik::{RouteSpec, TraefikError, render_file_provider_config},
};

pub struct DeploymentPlan {
    pub service: ServiceConfig,
    pub artifact: ArtifactRecord,
}

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("health error: {0}")]
    Health(#[from] HealthError),
    #[error("traefik error: {0}")]
    Traefik(#[from] TraefikError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bridge allocator lock poisoned")]
    BridgeLockPoisoned,
    #[error("bridge error: {0}")]
    Bridge(#[from] BridgeError),
    #[error("service does not use an external image source")]
    UnsupportedServiceSource,
    #[error("service does not use a git source")]
    UnsupportedGitSource,
    #[error("artifact acquisition error: {0}")]
    ArtifactAcquire(#[from] ArtifactAcquireError),
}

pub struct DeploymentCoordinator<R, H> {
    store: SqliteStore,
    runtime: R,
    health: H,
    routing: Option<RoutingState>,
}

struct RoutingState {
    bridge: Arc<Mutex<BridgeAllocator>>,
    routes: Arc<Mutex<BTreeMap<String, RouteSpec>>>,
    manager: Arc<dyn BridgeManager>,
    traefik_config_path: PathBuf,
}

impl<R, H> DeploymentCoordinator<R, H>
where
    R: Runtime,
    H: HealthChecker,
{
    pub fn new(store: SqliteStore, runtime: R, health: H) -> Self {
        Self {
            store,
            runtime,
            health,
            routing: None,
        }
    }

    pub fn new_with_routing(
        store: SqliteStore,
        runtime: R,
        health: H,
        bridge: BridgeAllocator,
        manager: Arc<dyn BridgeManager>,
        traefik_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_shared_routing(
            store,
            runtime,
            health,
            Arc::new(Mutex::new(bridge)),
            manager,
            traefik_config_path,
        )
    }

    pub fn new_with_shared_routing(
        store: SqliteStore,
        runtime: R,
        health: H,
        bridge: Arc<Mutex<BridgeAllocator>>,
        manager: Arc<dyn BridgeManager>,
        traefik_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            runtime,
            health,
            routing: Some(RoutingState {
                bridge,
                routes: Arc::new(Mutex::new(BTreeMap::new())),
                manager,
                traefik_config_path: traefik_config_path.into(),
            }),
        }
    }

    pub async fn deploy(&self, plan: DeploymentPlan) -> Result<Deployment, DeployError> {
        let mut deployment = self
            .store
            .create_deployment(deployment_request(&plan.service, &plan.artifact))?;

        let runtime_status = self
            .runtime
            .start(RuntimeStartRequest {
                service_name: plan.service.name.clone(),
                deployment_id: deployment.id,
                artifact: plan.artifact,
                internal_port: plan.service.internal_port,
                socket_path: format!("/var/lib/denia/runtime/{}/current.sock", plan.service.name)
                    .into(),
                cpu_millis: plan
                    .service
                    .resource_limits
                    .clone()
                    .unwrap_or_default()
                    .cpu_millis,
                memory_bytes: plan
                    .service
                    .resource_limits
                    .clone()
                    .unwrap_or_default()
                    .memory_bytes,
            })
            .await?;

        self.health
            .check(
                &format!("http://127.0.0.1:{}", plan.service.internal_port),
                &plan.service.health_check,
            )
            .await?;

        self.store
            .promote_deployment(plan.service.id, deployment.id)?;
        self.write_routing_config(&plan.service, &runtime_status.socket_path)
            .await?;
        self.store
            .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
        deployment.status = DeploymentStatus::Healthy;
        Ok(deployment)
    }

    pub async fn deploy_external_image_source(
        &self,
        service: &ServiceConfig,
        acquirer: &ArtifactAcquirer,
        runner: &dyn CommandRunner,
    ) -> Result<Deployment, DeployError> {
        let ServiceSource::ExternalImage(source) = &service.source else {
            return Err(DeployError::UnsupportedServiceSource);
        };
        let artifact = acquirer
            .acquire_rootfs_bundle_from_image_config(
                runner,
                ArtifactAcquireRequest::ExternalImage {
                    image: source.image.clone(),
                },
            )
            .await?;

        self.deploy(DeploymentPlan {
            service: service.clone(),
            artifact,
        })
        .await
    }

    pub async fn deploy_git_source(
        &self,
        service: &ServiceConfig,
        acquirer: &ArtifactAcquirer,
        runner: &dyn CommandRunner,
    ) -> Result<Deployment, DeployError> {
        let ServiceSource::Git(source) = &service.source else {
            return Err(DeployError::UnsupportedGitSource);
        };
        let artifact = acquirer
            .acquire_rootfs_bundle_from_image_config(
                runner,
                ArtifactAcquireRequest::Git {
                    repo_url: source.repo_url.clone(),
                    git_ref: source.git_ref.clone(),
                    dockerfile_path: source.dockerfile_path.clone(),
                    context_path: source.context_path.clone(),
                },
            )
            .await?;

        self.deploy(DeploymentPlan {
            service: service.clone(),
            artifact,
        })
        .await
    }

    pub async fn stop_service(&self, service: &ServiceConfig) -> Result<(), DeployError> {
        let promoted_deployment = self.store.promoted_deployment(service.id)?;

        self.runtime.stop(&service.name).await?;
        if let Some(routing) = &self.routing {
            routing.manager.deactivate(&service.name).await?;
            let yaml = {
                let mut routes = routing
                    .routes
                    .lock()
                    .map_err(|_| DeployError::BridgeLockPoisoned)?;
                routes.remove(&service.name);
                render_file_provider_config(&routes.values().cloned().collect::<Vec<_>>())?
            };
            std::fs::write(&routing.traefik_config_path, yaml)?;
        }

        if let Some(deployment_id) = promoted_deployment {
            self.store
                .update_deployment_status(deployment_id, DeploymentStatus::Stopped)?;
            self.store.clear_promoted_deployment(service.id)?;
        }
        Ok(())
    }

    async fn write_routing_config(
        &self,
        service: &ServiceConfig,
        socket_path: &std::path::Path,
    ) -> Result<(), DeployError> {
        let Some(routing) = &self.routing else {
            return Ok(());
        };
        let bridge_target = routing
            .bridge
            .lock()
            .map_err(|_| DeployError::BridgeLockPoisoned)?
            .assign(&service.name, socket_path.to_path_buf());
        routing.manager.activate(bridge_target.clone()).await?;
        let yaml = {
            let mut routes = routing
                .routes
                .lock()
                .map_err(|_| DeployError::BridgeLockPoisoned)?;
            routes.insert(
                service.name.clone(),
                RouteSpec {
                    service_name: service.name.clone(),
                    domains: service.domains.clone(),
                    bridge_port: bridge_target.port,
                },
            );
            render_file_provider_config(&routes.values().cloned().collect::<Vec<_>>())?
        };
        std::fs::write(&routing.traefik_config_path, yaml)?;
        Ok(())
    }
}

fn deployment_request(service: &ServiceConfig, artifact: &ArtifactRecord) -> DeploymentRequest {
    match &artifact.source {
        ArtifactSource::ExternalRegistry { image } => {
            DeploymentRequest::external_image(service.id, image.clone())
        }
        ArtifactSource::BuildKit {
            repo_url, git_ref, ..
        } => DeploymentRequest::Git {
            service_id: service.id,
            repo_url: repo_url.clone(),
            git_ref: git_ref.clone(),
        },
    }
}
