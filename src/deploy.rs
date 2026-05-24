use thiserror::Error;

use crate::{
    artifacts::{ArtifactRecord, ArtifactSource},
    domain::{Deployment, DeploymentRequest, DeploymentStatus, RuntimeStartRequest, ServiceConfig},
    health::{HealthChecker, HealthError},
    runtime::{Runtime, RuntimeError},
    state::{SqliteStore, StateError},
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
}

pub struct DeploymentCoordinator<R, H> {
    store: SqliteStore,
    runtime: R,
    health: H,
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
        self.store
            .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
        deployment.status = DeploymentStatus::Healthy;
        let _ = runtime_status;
        Ok(deployment)
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
