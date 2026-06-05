use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::project::Project;
use crate::domain::registry::validate_legacy_image_registry_host;
use crate::secrets::SecretRef;

/// Marker substituted for env values when redacting for lower-privilege callers.
pub const REDACTED_ENV_VALUE: &str = "***redacted***";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoscalePolicy {
    pub min_replicas: u32,
    pub max_replicas: u32,
    pub target_cpu_pct: u8,
    pub target_mem_pct: Option<u8>,
    pub scale_down_cooldown_s: u32,
    pub idle_timeout_s: u32,
}

impl AutoscalePolicy {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.max_replicas < 1 || self.min_replicas > self.max_replicas {
            return Err(DomainError::InvalidAutoscale("replica bounds".into()));
        }
        let pct_ok = |p: u8| (1..=100).contains(&p);
        if !pct_ok(self.target_cpu_pct) || self.target_mem_pct.is_some_and(|p| !pct_ok(p)) {
            return Err(DomainError::InvalidAutoscale("target percent".into()));
        }
        if self.idle_timeout_s < self.scale_down_cooldown_s {
            return Err(DomainError::InvalidAutoscale(
                "idle_timeout < cooldown".into(),
            ));
        }
        Ok(())
    }
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
    /// Upload-deployed service: the image is built from a working-tree context
    /// streamed up by `denia push`/`denia create` (ADR-039). It carries no
    /// service-level config — every deploy supplies its own build context as a
    /// `DeploymentRequest::Upload`, and the deploy path ignores `service.source`
    /// for uploads. Serializes as `{"type":"upload"}`.
    Upload,
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

/// Service names are used to derive filesystem paths (logs, sockets) and route
/// keys, so restrict them to a safe charset and reject empties. This is the
/// authoritative check enforced at API/storage boundaries.
pub fn validate_service_name(name: &str) -> Result<(), DomainError> {
    if name.trim().is_empty() {
        return Err(DomainError::EmptyName);
    }
    let safe = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if !safe {
        return Err(DomainError::InvalidName(name.to_string()));
    }
    Ok(())
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
        if legacy {
            validate_legacy_image_registry_host(&self.image)?;
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
    #[serde(default)]
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
    #[serde(default)]
    pub autoscale: Option<AutoscalePolicy>,
    #[serde(default)]
    pub endpoints: Vec<ServiceEndpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceEndpointProtocol {
    Http,
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub name: String,
    pub protocol: ServiceEndpointProtocol,
    pub internal_port: u16,
    #[serde(default)]
    pub public_port: Option<u16>,
}

impl ServiceEndpoint {
    pub fn http(name: impl Into<String>, internal_port: u16) -> Self {
        Self {
            name: name.into(),
            protocol: ServiceEndpointProtocol::Http,
            internal_port,
            public_port: None,
        }
    }

    pub fn tcp(name: impl Into<String>, internal_port: u16) -> Self {
        Self {
            name: name.into(),
            protocol: ServiceEndpointProtocol::Tcp,
            internal_port,
            public_port: None,
        }
    }

    pub fn udp(name: impl Into<String>, internal_port: u16) -> Self {
        Self {
            name: name.into(),
            protocol: ServiceEndpointProtocol::Udp,
            internal_port,
            public_port: None,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_endpoint_name(&self.name)?;
        if self.internal_port == 0 || self.public_port == Some(0) {
            return Err(DomainError::InvalidEndpointPort);
        }
        if self.protocol == ServiceEndpointProtocol::Http && self.public_port.is_some() {
            return Err(DomainError::HttpEndpointPublicPort);
        }
        Ok(())
    }
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
        let config = Self {
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
            autoscale: None,
            endpoints: Vec::new(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Validate every invariant of a service config. Safe to call on a config
    /// that was deserialized at an API boundary (which bypasses `new`).
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_service_name(&self.name)?;
        for domain in &self.domains {
            if let Err(e) = crate::verification::validate_hostname(domain) {
                return Err(DomainError::InvalidHostname(format!(
                    "domain '{domain}': {e}"
                )));
            }
        }
        if self.internal_port == 0 {
            return Err(DomainError::InvalidPort);
        }
        self.health_check.validate()?;
        match &self.source {
            ServiceSource::Git(git) => git.validate()?,
            ServiceSource::ExternalImage(img) => img.validate()?,
            // Nothing to validate: the build context is supplied per-deploy by
            // `denia push` (ADR-039), not stored on the service.
            ServiceSource::Upload => {}
        }
        if let Some(policy) = &self.autoscale {
            policy.validate()?;
        }
        for endpoint in &self.endpoints {
            endpoint.validate()?;
        }
        Ok(())
    }

    pub fn effective_endpoints(&self) -> Vec<ServiceEndpoint> {
        if self.endpoints.is_empty() {
            return vec![ServiceEndpoint::http("http", self.internal_port)];
        }
        self.endpoints.clone()
    }

    /// Replace every env *value* with a redaction marker, keeping keys so the
    /// UI can still show which variables exist. Used before returning configs to
    /// project members below Operator role (F-7).
    pub fn redact_env(&mut self) {
        for (_key, value) in self.env.iter_mut() {
            *value = REDACTED_ENV_VALUE.to_string();
        }
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

fn validate_endpoint_name(name: &str) -> Result<(), DomainError> {
    if name.trim().is_empty() {
        return Err(DomainError::EmptyEndpointName);
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(DomainError::InvalidEndpointName(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_config_deserializes_without_id_as_nil() {
        let json = r#"{
            "project_id": "0190b3a0-0000-7000-8000-000000000000",
            "name": "web",
            "domains": ["x.example.com"],
            "source": {"type": "external_image", "image": "nginx"},
            "internal_port": 80,
            "health_check": {"path": "/", "timeout_seconds": 5}
        }"#;
        let cfg: ServiceConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.id.is_nil());
        assert_eq!(cfg.name, "web");
    }

    #[test]
    fn service_config_deserializes_with_id_preserves_id() {
        let id = Uuid::now_v7();
        let json = format!(
            r#"{{
                "id": "{id}",
                "project_id": "0190b3a0-0000-7000-8000-000000000000",
                "name": "web",
                "domains": ["x.example.com"],
                "source": {{"type": "external_image", "image": "nginx"}},
                "internal_port": 80,
                "health_check": {{"path": "/", "timeout_seconds": 5}}
            }}"#
        );
        let cfg: ServiceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.id, id);
    }

    #[test]
    fn validate_rejects_unsafe_service_names() {
        assert_eq!(
            validate_service_name("../traefik").unwrap_err(),
            DomainError::InvalidName("../traefik".into())
        );
        assert_eq!(
            validate_service_name("web/x").unwrap_err(),
            DomainError::InvalidName("web/x".into())
        );
        assert_eq!(
            validate_service_name("").unwrap_err(),
            DomainError::EmptyName
        );
        assert!(validate_service_name("web-1_api").is_ok());
    }

    #[test]
    fn effective_endpoints_maps_legacy_port_to_default_http_endpoint() {
        let cfg = ServiceConfig::new(
            Uuid::now_v7(),
            "web",
            vec![],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/web:latest".to_string(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            8080,
            HealthCheck::new("/health", 5),
            None,
            vec![],
        )
        .expect("service");

        assert_eq!(
            cfg.effective_endpoints(),
            vec![ServiceEndpoint::http("http", 8080)]
        );
    }

    #[test]
    fn validate_accepts_tcp_and_udp_endpoints_without_public_ports() {
        let mut cfg = ServiceConfig::new(
            Uuid::now_v7(),
            "game",
            vec![],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/game:latest".to_string(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            7777,
            HealthCheck::new("/health", 5),
            None,
            vec![],
        )
        .expect("service");
        cfg.endpoints = vec![
            ServiceEndpoint::tcp("query", 27015),
            ServiceEndpoint::udp("gameplay", 7777),
        ];

        cfg.validate().expect("tcp/udp endpoints are valid");
    }

    #[test]
    fn validate_rejects_invalid_endpoint_shape() {
        let mut endpoint = ServiceEndpoint::tcp("bad name", 7777);
        assert!(matches!(
            endpoint.validate(),
            Err(DomainError::InvalidEndpointName(_))
        ));

        endpoint = ServiceEndpoint::udp("gameplay", 0);
        assert!(matches!(
            endpoint.validate(),
            Err(DomainError::InvalidEndpointPort)
        ));

        endpoint = ServiceEndpoint::http("http", 8080);
        endpoint.public_port = Some(8080);
        assert!(matches!(
            endpoint.validate(),
            Err(DomainError::HttpEndpointPublicPort)
        ));
    }

    #[test]
    fn validate_rejects_absolute_and_parent_git_build_paths() {
        let mk = |ctx: &str, dockerfile: &str| ServiceConfig {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "web".into(),
            domains: vec!["x.example.com".into()],
            source: ServiceSource::Git(GitSource {
                repo_url: "https://example.com/acme/api.git".into(),
                git_ref: "main".into(),
                dockerfile_path: dockerfile.into(),
                context_path: ctx.into(),
                credential: SecretRef::new("deploy-key"),
            }),
            internal_port: 80,
            health_check: HealthCheck::new("/health", 5),
            resource_limits: None,
            env: Vec::new(),
            tls_enabled: false,
            autoscale: None,
            endpoints: Vec::new(),
        };
        assert!(matches!(
            mk("/var/lib/denia", "Dockerfile").validate().unwrap_err(),
            DomainError::InvalidGitBuildPath { .. }
        ));
        assert!(matches!(
            mk(".", "../../etc").validate().unwrap_err(),
            DomainError::InvalidGitBuildPath { .. }
        ));
        assert!(mk(".", "Dockerfile").validate().is_ok());
    }

    #[test]
    fn autoscale_policy_validates_bounds() {
        let ok = AutoscalePolicy {
            min_replicas: 0,
            max_replicas: 3,
            target_cpu_pct: 80,
            target_mem_pct: Some(75),
            scale_down_cooldown_s: 300,
            idle_timeout_s: 600,
        };
        assert!(ok.validate().is_ok());
        assert!(
            AutoscalePolicy {
                min_replicas: 5,
                max_replicas: 2,
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            AutoscalePolicy {
                idle_timeout_s: 100,
                scale_down_cooldown_s: 300,
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            AutoscalePolicy {
                target_cpu_pct: 0,
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validate_allows_empty_domains() {
        let cfg = ServiceConfig::new(
            uuid::Uuid::now_v7(),
            "no-domain-svc",
            vec![],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "nginx:latest".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            HealthCheck::new("/", 5),
            None,
            vec![],
        );
        assert!(cfg.is_ok(), "empty domains must be valid: {cfg:?}");
    }

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

    #[test]
    fn external_image_source_rejects_local_explicit_registry_hosts() {
        for image in [
            "localhost:5000/acme/web:1",
            "127.0.0.1:5000/acme/web:1",
            "10.0.0.5/acme/web:1",
            "169.254.169.254/latest:tag",
        ] {
            let source = ExternalImageSource {
                image: image.to_string(),
                credential: None,
                registry_id: None,
                image_ref: None,
            };
            assert!(source.validate().is_err(), "{image} should be rejected");
        }

        ExternalImageSource {
            image: "busybox:latest".to_string(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }
        .validate()
        .expect("default registry shorthand remains valid");
    }

    #[test]
    fn upload_source_serde_round_trips_and_validates() {
        // The unit variant serializes to the tagged form `{"type":"upload"}`.
        let json = serde_json::to_string(&ServiceSource::Upload).unwrap();
        assert_eq!(json, r#"{"type":"upload"}"#);
        let back: ServiceSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ServiceSource::Upload);

        // A service with an upload source validates: the build context is
        // supplied per-deploy by `denia push` (ADR-039), not on the service.
        let cfg = ServiceConfig::new(
            uuid::Uuid::now_v7(),
            "upload-svc",
            vec![],
            ServiceSource::Upload,
            8080,
            HealthCheck::new("/", 5),
            None,
            vec![],
        );
        assert!(cfg.is_ok(), "upload source must validate: {cfg:?}");
    }
}
