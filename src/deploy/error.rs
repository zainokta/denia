use thiserror::Error;

use crate::{
    artifacts::acquirer::ArtifactAcquireError, bridge::BridgeError, health::HealthError,
    runtime::RuntimeError, state::StateError, traefik::TraefikError,
};

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
    #[error("bridge port pool exhausted")]
    BridgePortExhausted,
    #[error("service does not use an external image source")]
    UnsupportedServiceSource,
    #[error("service does not use a git source")]
    UnsupportedGitSource,
    #[error("artifact acquisition error: {0}")]
    ArtifactAcquire(#[from] ArtifactAcquireError),
    #[error("registry not found")]
    RegistryNotFound,
    #[error("secret decrypt: {0}")]
    SecretDecrypt(#[from] crate::secrets::SecretError),
    #[error("registry auth resolution: {0}")]
    RegistryAuthResolution(crate::oci::OciError),
}
