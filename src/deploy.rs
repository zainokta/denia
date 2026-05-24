use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::{
    artifacts::{ArtifactRecord, ArtifactSource},
    bridge::BridgeAllocator,
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
}

pub struct DeploymentCoordinator<R, H> {
    store: SqliteStore,
    runtime: R,
    health: H,
    routing: Option<RoutingState>,
}

struct RoutingState {
    bridge: Arc<Mutex<BridgeAllocator>>,
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
        traefik_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            runtime,
            health,
            routing: Some(RoutingState {
                bridge: Arc::new(Mutex::new(bridge)),
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
                cpu_millis: plan.service.resource_limits.cpu_millis,
                memory_bytes: plan.service.resource_limits.memory_bytes,
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
        self.write_routing_config(&plan.service, &runtime_status.socket_path)?;
        self.store
            .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
        deployment.status = DeploymentStatus::Healthy;
        Ok(deployment)
    }

    fn write_routing_config(
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
        let yaml = render_file_provider_config(&[RouteSpec {
            service_name: service.name.clone(),
            domains: service.domains.clone(),
            bridge_port: bridge_target.port,
        }])?;
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
