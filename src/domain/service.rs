use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::project::Project;
use crate::secrets::SecretRef;

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
    #[serde(default)]
    pub registry_id: Option<Uuid>,
    #[serde(default)]
    pub image_ref: Option<String>,
}

impl GitSource {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_build_path(&self.context_path, "context_path")?;
        validate_build_path(&self.dockerfile_path, "dockerfile_path")?;
        Ok(())
    }
}

fn validate_build_path(path: &str, field: &str) -> Result<(), DomainError> {
    if path.is_empty() {
        return Err(DomainError::InvalidGitBuildPath {
            field: field.to_string(),
            reason: "path must not be empty".to_string(),
        });
    }
    if path.starts_with('/') {
        return Err(DomainError::InvalidGitBuildPath {
            field: field.to_string(),
            reason: "path must not be absolute".to_string(),
        });
    }
    if path.contains("..") {
        return Err(DomainError::InvalidGitBuildPath {
            field: field.to_string(),
            reason: "path must not contain parent directory reference".to_string(),
        });
    }
    if path.contains('\0') || path.contains('\n') || path.contains('\r') {
        return Err(DomainError::InvalidGitBuildPath {
            field: field.to_string(),
            reason: "path contains invalid characters".to_string(),
        });
    }
    Ok(())
}

impl ExternalImageSource {
    fn uses_registry(&self) -> bool {
        self.registry_id.is_some() || self.image_ref.is_some()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let registry = self.registry_id.is_some() && self.image_ref.is_some();
        let partial_registry = self.uses_registry() && !registry;
        let legacy = !self.image.trim().is_empty();
        if registry && legacy {
            return Err(DomainError::RegistrySourceAmbiguous);
        }
        if partial_registry {
            return Err(DomainError::RegistrySourceMissing);
        }
        if !registry && !legacy {
            return Err(DomainError::RegistrySourceMissing);
        }
        Ok(())
    }

    /// Returns (full_image_ref, used_registry).
    /// `endpoint` is only used on the registry path; ignored for the legacy path.
    pub fn resolve_ref(&self, endpoint: &str) -> Result<(String, bool), DomainError> {
        self.validate()?;
        if let (Some(_), Some(image_ref)) = (self.registry_id, &self.image_ref) {
            Ok((
                format!("{}/{}", endpoint.trim_end_matches('/'), image_ref),
                true,
            ))
        } else {
            Ok((self.image.clone(), false))
        }
    }
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
    #[serde(default)]
    pub tls_enabled: bool,
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
        for domain in &domains {
            if let Err(e) = crate::domains::validate_hostname(domain) {
                return Err(DomainError::InvalidHostname(format!(
                    "domain '{domain}': {e}"
                )));
            }
        }
        if internal_port == 0 {
            return Err(DomainError::InvalidPort);
        }
        health_check.validate()?;
        if let ServiceSource::Git(git) = &source {
            git.validate()?;
        }
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
            tls_enabled: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_image_source_resolution_matrix() {
        // legacy: full image only
        let legacy = ExternalImageSource {
            image: "ghcr.io/acme/web:1".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        };
        let (full, used_registry) = legacy.resolve_ref("docker.io").unwrap();
        assert_eq!(full, "ghcr.io/acme/web:1");
        assert!(!used_registry);

        // new: registry + image_ref
        let new = ExternalImageSource {
            image: String::new(),
            credential: None,
            registry_id: Some(Uuid::now_v7()),
            image_ref: Some("library/redis:7".into()),
        };
        let (full, used_registry) = new.resolve_ref("docker.io").unwrap();
        assert_eq!(full, "docker.io/library/redis:7");
        assert!(used_registry);

        // ambiguous: both
        let both = ExternalImageSource {
            image: "x".into(),
            credential: None,
            registry_id: Some(Uuid::now_v7()),
            image_ref: Some("y".into()),
        };
        assert_eq!(
            both.validate().unwrap_err(),
            DomainError::RegistrySourceAmbiguous
        );

        // missing: neither
        let neither = ExternalImageSource {
            image: String::new(),
            credential: None,
            registry_id: None,
            image_ref: None,
        };
        assert_eq!(
            neither.validate().unwrap_err(),
            DomainError::RegistrySourceMissing
        );

        // partial: only registry_id set -> missing
        let only_registry_id = ExternalImageSource {
            image: String::new(),
            credential: None,
            registry_id: Some(Uuid::now_v7()),
            image_ref: None,
        };
        assert_eq!(
            only_registry_id.validate().unwrap_err(),
            DomainError::RegistrySourceMissing
        );

        // partial: only image_ref set -> missing
        let only_image_ref = ExternalImageSource {
            image: String::new(),
            credential: None,
            registry_id: None,
            image_ref: Some("library/redis:7".into()),
        };
        assert_eq!(
            only_image_ref.validate().unwrap_err(),
            DomainError::RegistrySourceMissing
        );
    }
}
