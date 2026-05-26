//! Deployment + artifact repository trait.

use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::domain::{Deployment, DeploymentRequest, DeploymentStatus};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait DeploymentRepo: Send + Sync + 'static {
    fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, RepoError>;
    fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, RepoError>;
    fn update_deployment_status(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
    ) -> Result<(), RepoError>;
    fn promote_deployment(&self, service_id: Uuid, deployment_id: Uuid) -> Result<(), RepoError>;
    fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, RepoError>;
    fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), RepoError>;
    fn put_artifact(&self, artifact: ArtifactRecord) -> Result<ArtifactRecord, RepoError>;
    fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>, RepoError>;
}
