use thiserror::Error;

use crate::{
    artifacts::acquirer::ArtifactAcquireError, health::HealthError, ingress::pingora::IngressError,
    repo::RepoError, runtime::RuntimeError, state::StateError,
};

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("repo error: {0}")]
    Repo(#[from] RepoError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("health error: {0}")]
    Health(#[from] HealthError),
    #[error("ingress error: {0}")]
    Ingress(#[from] IngressError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ingress routes lock poisoned")]
    RoutesLockPoisoned,
    #[error("service does not use an external image source")]
    UnsupportedServiceSource,
    #[error("service does not use a git source")]
    UnsupportedGitSource,
    #[error("no existing artifact to redeploy; deploy the service first")]
    NoExistingArtifact,
    #[error("artifact acquisition error: {0}")]
    ArtifactAcquire(#[from] ArtifactAcquireError),
    #[error("registry not found")]
    RegistryNotFound,
    #[error("secret decrypt: {0}")]
    SecretDecrypt(#[from] crate::secrets::SecretError),
    #[error("registry auth resolution: {0}")]
    RegistryAuthResolution(crate::oci::OciError),
}
