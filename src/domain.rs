use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::secrets::SecretRef;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("service name cannot be empty")]
    EmptyName,
    #[error("service must have at least one domain")]
    MissingDomain,
    #[error("internal port must be between 1 and 65535")]
    InvalidPort,
    #[error("health check path must start with /")]
    InvalidHealthPath,
    #[error("health check timeout must be greater than zero")]
    InvalidHealthTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_millis: 500,
            memory_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub path: String,
    pub timeout_seconds: u64,
}

impl HealthCheck {
    pub fn new(path: impl Into<String>, timeout_seconds: u64) -> Self {
        Self {
            path: path.into(),
            timeout_seconds,
        }
    }

    fn validate(&self) -> Result<(), DomainError> {
        if !self.path.starts_with('/') {
            return Err(DomainError::InvalidHealthPath);
        }
        if self.timeout_seconds == 0 {
            return Err(DomainError::InvalidHealthTimeout);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceSource {
    Git(GitSource),
    ExternalImage(ExternalImageSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSource {
    pub repo_url: String,
    pub git_ref: String,
    pub dockerfile_path: String,
    pub context_path: String,
    pub credential: SecretRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalImageSource {
    pub image: String,
    pub credential: Option<SecretRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub domains: Vec<String>,
    pub source: ServiceSource,
    pub internal_port: u16,
    pub health_check: HealthCheck,
    #[serde(default)]
    pub resource_limits: Option<ResourceLimits>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

impl ServiceConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: Uuid,
        name: impl Into<String>,
        domains: Vec<String>,
        source: ServiceSource,
        internal_port: u16,
        health_check: HealthCheck,
        resource_limits: Option<ResourceLimits>,
        env: Vec<(String, String)>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        if domains.is_empty() {
            return Err(DomainError::MissingDomain);
        }
        if internal_port == 0 {
            return Err(DomainError::InvalidPort);
        }
        health_check.validate()?;
        Ok(Self {
            id: Uuid::now_v7(),
            project_id,
            name,
            domains,
            source,
            internal_port,
            health_check,
            resource_limits,
            env,
        })
    }

    pub fn effective_env(&self, project: &Project) -> BTreeMap<String, String> {
        let mut map: BTreeMap<String, String> = project.shared_env.iter().cloned().collect();
        map.extend(self.env.iter().cloned());
        map
    }

    pub fn effective_limits(&self, project: &Project) -> ResourceLimits {
        self.resource_limits
            .clone()
            .or_else(|| project.default_resource_limits.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    SshDeployKey,
    RegistryBasic,
    RegistryToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub kind: CredentialKind,
    pub secret_ref: SecretRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum DeploymentRequest {
    Git {
        service_id: Uuid,
        repo_url: String,
        git_ref: String,
    },
    ExternalImage {
        service_id: Uuid,
        image: String,
    },
}

impl DeploymentRequest {
    pub fn service_id(&self) -> Uuid {
        match self {
            Self::Git { service_id, .. } | Self::ExternalImage { service_id, .. } => *service_id,
        }
    }

    pub fn external_image(service_id: Uuid, image: impl Into<String>) -> Self {
        Self::ExternalImage {
            service_id,
            image: image.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentStatus {
    Pending,
    Building,
    Starting,
    Healthy,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub request: DeploymentRequest,
    pub status: DeploymentStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStartRequest {
    pub service_name: String,
    pub service_id: Uuid,
    pub deployment_id: Uuid,
    pub artifact: crate::artifacts::ArtifactRecord,
    pub internal_port: u16,
    pub socket_path: std::path::PathBuf,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub service_name: String,
    pub deployment_id: Uuid,
    pub state: String,
    pub pid: Option<u32>,
    pub cgroup_path: std::path::PathBuf,
    pub socket_path: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub shared_env: Vec<(String, String)>,
    #[serde(default)]
    pub default_resource_limits: Option<ResourceLimits>,
    pub created_at: DateTime<Utc>,
}

impl Project {
    pub fn new(name: impl Into<String>, description: Option<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            name,
            description,
            shared_env: Vec::new(),
            default_resource_limits: None,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer = 0,
    Operator = 1,
    Admin = 2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_super_admin: bool,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub fn new(
        username: impl Into<String>,
        password_hash: String,
        is_super_admin: bool,
    ) -> Result<Self, DomainError> {
        let username = username.into();
        if username.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            username,
            password_hash,
            is_super_admin,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMembership {
    pub user_id: Uuid,
    pub project_id: Uuid,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token_hash: String,
    pub user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrincipalView {
    User { user: User },
    Bootstrap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Me {
    pub principal: PrincipalView,
    pub is_super_admin: bool,
    pub memberships: Vec<ProjectMembership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResult {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rejects_empty_name() {
        assert_eq!(Project::new("", None).unwrap_err(), DomainError::EmptyName);
    }

    #[test]
    fn project_has_id_and_defaults() {
        let p = Project::new("default", Some("seed".into())).unwrap();
        assert_eq!(p.name, "default");
        assert!(p.shared_env.is_empty());
        assert!(p.default_resource_limits.is_none());
    }
}
