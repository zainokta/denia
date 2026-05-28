use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("service name cannot be empty")]
    EmptyName,
    #[error("invalid service name '{0}': only ASCII alphanumeric, '-' and '_' are allowed")]
    InvalidName(String),
    #[error("internal port must be between 1 and 65535")]
    InvalidPort,
    #[error("health check path must start with /")]
    InvalidHealthPath,
    #[error("health check timeout must be greater than zero")]
    InvalidHealthTimeout,
    #[error("invalid cron schedule")]
    InvalidSchedule,
    #[error("invalid hostname: {0}")]
    InvalidHostname(String),
    #[error("registry endpoint cannot be empty")]
    RegistryMissingEndpoint,
    #[error("registry credential is required for non-anonymous auth")]
    RegistryMissingCredential,
    #[error("external image source has both legacy image and registry_id/image_ref set")]
    RegistrySourceAmbiguous,
    #[error("external image source requires either image or both registry_id and image_ref")]
    RegistrySourceMissing,
    #[error("invalid git build path field '{field}': {reason}")]
    InvalidGitBuildPath { field: String, reason: String },
    #[error("invalid autoscale policy: {0}")]
    InvalidAutoscale(String),
}
