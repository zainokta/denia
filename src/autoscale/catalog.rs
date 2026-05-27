//! Repo-backed [`ServiceCatalog`].
//!
//! Assembles a [`ManagedService`] launch context for each autoscaled service
//! from persisted state. A service is "managed" only when it has an autoscale
//! policy, a promoted deployment, and that deployment is linked to an artifact;
//! services missing any of these are silently excluded so the controller never
//! tries to launch something it cannot fully describe.

use crate::autoscale::controller::{ManagedService, ServiceCatalog};
use crate::domain::ServiceConfig;
use crate::repo::sqlite::{SqliteDeploymentRepo, SqliteProjectRepo, SqliteServiceRepo};

pub struct RepoServiceCatalog {
    services: SqliteServiceRepo,
    projects: SqliteProjectRepo,
    deployments: SqliteDeploymentRepo,
}

impl RepoServiceCatalog {
    pub fn new(
        services: SqliteServiceRepo,
        projects: SqliteProjectRepo,
        deployments: SqliteDeploymentRepo,
    ) -> Self {
        Self {
            services,
            projects,
            deployments,
        }
    }

    /// Build a [`ManagedService`] from a persisted [`ServiceConfig`]. Returns
    /// `None` (excluding the service) unless it is autoscaled, has a promoted
    /// deployment, that deployment is linked to an artifact, and its project
    /// resolves.
    fn build(&self, svc: &ServiceConfig) -> Option<ManagedService> {
        let policy = svc.autoscale.clone()?;
        let deployment_id = self
            .deployments
            .promoted_deployment(svc.id)
            .ok()
            .flatten()?;
        let artifact = self
            .deployments
            .get_deployment_artifact(deployment_id)
            .ok()
            .flatten()?;
        let project = self.projects.get_project(svc.project_id).ok().flatten()?;
        let limits = svc.effective_limits(&project);
        let env = svc.effective_env(&project).into_iter().collect();
        Some(ManagedService {
            service_name: svc.name.clone(),
            service_id: svc.id,
            deployment_id,
            policy,
            artifact,
            internal_port: svc.internal_port,
            limits,
            env,
            health_check: svc.health_check.clone(),
        })
    }
}

impl ServiceCatalog for RepoServiceCatalog {
    fn resolve(&self, service_key: &str) -> Option<ManagedService> {
        // The bridge identity is the service_id (globally unique), not the name
        // (project-scoped) — see F-3. Resolve by parsed id.
        let service_id = uuid::Uuid::parse_str(service_key).ok()?;
        let svc = self
            .services
            .list_services()
            .ok()?
            .into_iter()
            .find(|s| s.id == service_id)?;
        self.build(&svc)
    }

    fn all(&self) -> Vec<ManagedService> {
        self.services
            .list_services()
            .unwrap_or_default()
            .iter()
            .filter_map(|s| self.build(s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::domain::DeploymentRequest;
    use crate::domain::{
        AutoscalePolicy, ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource,
    };
    use crate::state::SqliteStore;
    use uuid::Uuid;

    struct Fixture {
        catalog: RepoServiceCatalog,
        services: SqliteServiceRepo,
        deployments: SqliteDeploymentRepo,
        project_id: Uuid,
    }

    fn fixture() -> Fixture {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let pool = store.pool();
        let services = SqliteServiceRepo::new(pool.clone());
        let projects = SqliteProjectRepo::new(pool.clone());
        let deployments = SqliteDeploymentRepo::new(pool);

        let project = projects
            .put_project(Project::new("proj", None).unwrap())
            .unwrap();
        let project_id = project.id;

        let catalog = RepoServiceCatalog::new(services.clone(), projects, deployments.clone());
        Fixture {
            catalog,
            services,
            deployments,
            project_id,
        }
    }

    fn policy() -> AutoscalePolicy {
        AutoscalePolicy {
            min_replicas: 1,
            max_replicas: 3,
            target_cpu_pct: 80,
            target_mem_pct: None,
            scale_down_cooldown_s: 300,
            idle_timeout_s: 600,
        }
    }

    fn service(project_id: Uuid, name: &str, autoscale: Option<AutoscalePolicy>) -> ServiceConfig {
        let mut svc = ServiceConfig::new(
            project_id,
            name,
            vec![format!("{name}.example.com")],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/web:1".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            8080,
            HealthCheck::new("/healthz", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        svc.autoscale = autoscale;
        svc
    }

    /// Persist a deployment for `service_id`, link an artifact with the given
    /// digest, and promote it. Returns the deployment id.
    fn promote_with_artifact(f: &Fixture, service_id: Uuid, digest: &str) -> Uuid {
        let dep = f
            .deployments
            .create_deployment(DeploymentRequest::external_image(service_id, "img"))
            .unwrap();
        let record = ArtifactRecord::new(
            digest,
            ArtifactKind::OciImage,
            ArtifactSource::ExternalRegistry {
                image: "ghcr.io/acme/web:1".to_string(),
            },
        )
        .unwrap();
        f.deployments.put_artifact(record).unwrap();
        f.deployments
            .set_deployment_artifact(dep.id, digest)
            .unwrap();
        f.deployments
            .promote_deployment(service_id, dep.id)
            .unwrap();
        dep.id
    }

    #[test]
    fn catalog_all_returns_autoscaled_service() {
        let f = fixture();
        let svc = service(f.project_id, "web", Some(policy()));
        f.services.put_service(svc.clone()).unwrap();
        let dep_id = promote_with_artifact(&f, svc.id, "sha256:deadbeef");

        let all = f.catalog.all();
        assert_eq!(all.len(), 1);
        let ms = &all[0];
        assert_eq!(ms.service_id, svc.id);
        assert_eq!(ms.deployment_id, dep_id);
        assert_eq!(ms.artifact.digest, "sha256:deadbeef");
        assert_eq!(ms.service_name, "web");
    }

    #[test]
    fn catalog_excludes_service_without_policy() {
        let f = fixture();
        let autoscaled = service(f.project_id, "web", Some(policy()));
        f.services.put_service(autoscaled.clone()).unwrap();
        promote_with_artifact(&f, autoscaled.id, "sha256:aa");

        // A second service WITHOUT an autoscale policy, fully promoted.
        let plain = service(f.project_id, "plain", None);
        f.services.put_service(plain.clone()).unwrap();
        promote_with_artifact(&f, plain.id, "sha256:bb");

        let all = f.catalog.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].service_id, autoscaled.id);
    }

    #[test]
    fn catalog_excludes_when_no_promoted_deployment() {
        let f = fixture();
        let svc = service(f.project_id, "web", Some(policy()));
        f.services.put_service(svc.clone()).unwrap();
        // No promoted deployment.

        assert!(f.catalog.all().is_empty());
    }

    #[test]
    fn catalog_resolve_by_id() {
        let f = fixture();
        let svc = service(f.project_id, "web", Some(policy()));
        f.services.put_service(svc.clone()).unwrap();
        promote_with_artifact(&f, svc.id, "sha256:cc");

        let ms = f.catalog.resolve(&svc.id.to_string()).expect("resolved");
        assert_eq!(ms.service_id, svc.id);
        assert!(f.catalog.resolve(&Uuid::now_v7().to_string()).is_none());
        assert!(f.catalog.resolve("not-a-uuid").is_none());
    }
}
