use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use uuid::Uuid;

use crate::artifacts::acquirer::{ArtifactAcquireRequest, ArtifactAcquirer};
use crate::artifacts::{ArtifactRecord, ArtifactSource};
use crate::command::CommandRunner;
use crate::deploy::error::DeployError;
use crate::deploy::routes::{SharedRoutes, route_table_from_snapshot};
use crate::domain::ServiceSource;
use crate::domain::{
    Deployment, DeploymentRequest, DeploymentStatus, RuntimeInstanceId, RuntimeStartRequest,
    ServiceConfig,
};
use crate::health::HealthChecker;
use crate::ingress::pingora::{IngressState, RouteSpec};
use crate::oci::RegistryAuth;
use crate::repo::RepoError;
use crate::repo::sqlite::{
    SqliteDeploymentRepo, SqliteDomainRepo, SqliteProjectRepo, SqliteRegistryRepo,
};
use crate::runtime::Runtime;

/// Stable replica id for the single endpoint the deploy path registers for a
/// service. The deploy coordinator manages one promoted replica per service; the
/// autoscaler owns multi-replica fan-out via its own (UUIDv7) ids. A fixed
/// nil-derived id keeps this endpoint addressable and replaceable on re-deploy.
const DEPLOY_REPLICA_ID: Uuid = Uuid::nil();

pub struct DeploymentPlan {
    pub service: ServiceConfig,
    pub artifact: ArtifactRecord,
}

/// Bundle of repos used by `DeploymentCoordinator`.
///
/// Keeps the coordinator constructor signature short by grouping the four
/// aggregates it needs (deployments, projects, registries, domains).
#[derive(Clone)]
pub struct DeploymentRepos {
    pub deployments: SqliteDeploymentRepo,
    pub projects: SqliteProjectRepo,
    pub registries: SqliteRegistryRepo,
    pub domains: SqliteDomainRepo,
}

pub struct DeploymentCoordinator<R, H> {
    repos: DeploymentRepos,
    runtime: R,
    health: H,
    routing: Option<RoutingState>,
}

struct RoutingState {
    ingress: Arc<IngressState>,
    routes: SharedRoutes,
}

impl<R, H> DeploymentCoordinator<R, H>
where
    R: Runtime,
    H: HealthChecker,
{
    pub fn new(repos: DeploymentRepos, runtime: R, health: H) -> Self {
        Self {
            repos,
            runtime,
            health,
            routing: None,
        }
    }

    pub fn new_with_routing(
        repos: DeploymentRepos,
        runtime: R,
        health: H,
        ingress: Arc<IngressState>,
    ) -> Self {
        Self::new_with_shared_routing(
            repos,
            runtime,
            health,
            ingress,
            Arc::new(Mutex::new(BTreeMap::new())),
        )
    }

    pub fn new_with_shared_routing(
        repos: DeploymentRepos,
        runtime: R,
        health: H,
        ingress: Arc<IngressState>,
        routes: SharedRoutes,
    ) -> Self {
        Self {
            repos,
            runtime,
            health,
            routing: Some(RoutingState { ingress, routes }),
        }
    }

    /// Persist a `Pending` deployment row up front so the API can return it
    /// immediately while the rest of the pipeline runs asynchronously.
    ///
    /// The `service` argument is reserved for future per-service validation
    /// (e.g. rejecting deploys for stopped services); today it is unused.
    pub async fn create_pending(
        &self,
        service: &ServiceConfig,
        request: DeploymentRequest,
    ) -> Result<Deployment, DeployError> {
        let _ = service;
        let deployment = self.repos.deployments.create_deployment(request)?;
        Ok(deployment)
    }

    /// Drive a previously-`Pending` deployment through Building → Starting →
    /// Healthy, emitting per-phase log lines via `log`. On failure, writes an
    /// `ERROR` log line and transitions the row to `Failed`.
    pub async fn run(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<(), DeployError> {
        let res = self.run_inner(deployment_id, service, request, log).await;
        if let Err(ref e) = res {
            let _ = log.write("ERROR", &format!("{e:?}"));
            let _ = self
                .repos
                .deployments
                .update_deployment_status(deployment_id, DeploymentStatus::Failed);
        }
        res
    }

    async fn run_inner(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<(), DeployError> {
        log.write("START", &format!("deployment_id={deployment_id}"))
            .ok();

        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Building)?;
        log.write("BUILDING", "acquiring artifact").ok();

        let artifact = self
            .acquire_artifact_for_run(&service, &request, log)
            .await?;
        self.repos.deployments.put_artifact(artifact.clone())?;
        self.repos
            .deployments
            .set_deployment_artifact(deployment_id, &artifact.digest)?;

        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Starting)?;
        log.write("STARTING", "launching runtime").ok();

        self.finalize(deployment_id, &service, artifact, log)
            .await?;

        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Healthy)?;
        log.write("HEALTHY", "deployment promoted").ok();
        Ok(())
    }

    /// STUB: Task 4 wires the acquirer + runner + secret store + sops_binary
    /// dependencies through `run_with_deps` so this method can resolve the
    /// service source (ExternalImage / Git) into an `ArtifactRecord`. Until
    /// then, callers of `run` MUST NOT exercise this path — tests that drive
    /// `run` end-to-end are marked `#[ignore]`.
    async fn acquire_artifact_for_run(
        &self,
        service: &ServiceConfig,
        request: &DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<ArtifactRecord, DeployError> {
        let _ = (service, request, log);
        unimplemented!("filled in by Task 4")
    }

    /// Runtime-start + healthcheck + promote + write_routing_config portion of
    /// the deploy pipeline. Mirrors the body of `deploy()` but uses the
    /// supplied `deployment_id` (no `create_deployment` here) and emits log
    /// lines for the SSE viewer.
    async fn finalize(
        &self,
        deployment_id: Uuid,
        service: &ServiceConfig,
        artifact: ArtifactRecord,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<(), DeployError> {
        let project = self
            .repos
            .projects
            .get_project(service.project_id)?
            .ok_or(DeployError::Repo(RepoError::UnknownProject))?;
        let limits = service.effective_limits(&project);
        let env: Vec<(String, String)> = service.effective_env(&project).into_iter().collect();

        log.write("RUNTIME_START", &format!("port={}", service.internal_port))
            .ok();
        let runtime_status = self
            .runtime
            .start(RuntimeStartRequest {
                service_name: service.name.clone(),
                service_id: service.id,
                deployment_id,
                artifact,
                internal_port: service.internal_port,
                socket_path: format!("/var/lib/denia/runtime/{}/current.sock", service.id).into(),
                cpu_millis: limits.cpu_millis,
                memory_bytes: limits.memory_bytes,
                env,
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
                replica_index: 0,
            })
            .await?;

        log.write("HEALTHCHECK", "starting").ok();
        self.health
            .check(
                &format!("http://127.0.0.1:{}", service.internal_port),
                &service.health_check,
            )
            .await?;
        log.write("HEALTHCHECK", "passed").ok();

        self.repos
            .deployments
            .promote_deployment(service.id, deployment_id)?;
        self.write_routing_config(service, &runtime_status.socket_path)
            .await?;
        Ok(())
    }

    pub async fn deploy(&self, plan: DeploymentPlan) -> Result<Deployment, DeployError> {
        let mut deployment = self
            .repos
            .deployments
            .create_deployment(deployment_request(&plan.service, &plan.artifact))?;

        self.repos.deployments.put_artifact(plan.artifact.clone())?;
        self.repos
            .deployments
            .set_deployment_artifact(deployment.id, &plan.artifact.digest)?;

        let project = self
            .repos
            .projects
            .get_project(plan.service.project_id)?
            .ok_or(DeployError::Repo(RepoError::UnknownProject))?;
        let limits = plan.service.effective_limits(&project);
        let env: Vec<(String, String)> = plan.service.effective_env(&project).into_iter().collect();

        let runtime_status = self
            .runtime
            .start(RuntimeStartRequest {
                service_name: plan.service.name.clone(),
                service_id: plan.service.id,
                deployment_id: deployment.id,
                artifact: plan.artifact,
                internal_port: plan.service.internal_port,
                socket_path: format!("/var/lib/denia/runtime/{}/current.sock", plan.service.id)
                    .into(),
                cpu_millis: limits.cpu_millis,
                memory_bytes: limits.memory_bytes,
                env,
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
                replica_index: 0,
            })
            .await?;

        self.health
            .check(
                &format!("http://127.0.0.1:{}", plan.service.internal_port),
                &plan.service.health_check,
            )
            .await?;

        self.repos
            .deployments
            .promote_deployment(plan.service.id, deployment.id)?;
        self.write_routing_config(&plan.service, &runtime_status.socket_path)
            .await?;
        self.repos
            .deployments
            .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
        deployment.status = DeploymentStatus::Healthy;
        Ok(deployment)
    }

    pub async fn deploy_external_image_source(
        &self,
        service: &ServiceConfig,
        acquirer: &ArtifactAcquirer,
        runner: &dyn CommandRunner,
        secret_store: &crate::secrets::SopsSecretStore,
        sops_binary: &std::path::Path,
    ) -> Result<Deployment, DeployError> {
        let ServiceSource::ExternalImage(source) = &service.source else {
            return Err(DeployError::UnsupportedServiceSource);
        };

        let (full_ref, auth) = if let Some(registry_id) = source.registry_id {
            let registry = self
                .repos
                .registries
                .registry(registry_id)?
                .ok_or(DeployError::RegistryNotFound)?;
            let payload = match &registry.credential_ref {
                Some(secret_ref) => Some(
                    secret_store
                        .decrypt(runner, sops_binary, registry.project_id, secret_ref)
                        .await?,
                ),
                None => None,
            };
            let auth = crate::oci::credentials::resolve_registry_auth(
                registry.auth_kind,
                payload.as_ref(),
            )
            .map_err(DeployError::RegistryAuthResolution)?;
            let (full_ref, _) = source
                .resolve_ref(&registry.endpoint)
                .map_err(|_| DeployError::UnsupportedServiceSource)?;
            (full_ref, auth)
        } else {
            let (full_ref, _) = source
                .resolve_ref("")
                .map_err(|_| DeployError::UnsupportedServiceSource)?;
            let auth = match &source.credential {
                Some(secret_ref) => {
                    let payload = secret_store
                        .decrypt(runner, sops_binary, service.project_id, secret_ref)
                        .await?;
                    crate::oci::credentials::resolve_registry_auth(
                        crate::domain::RegistryAuthKind::Basic,
                        Some(&payload),
                    )
                    .map_err(DeployError::RegistryAuthResolution)?
                }
                None => RegistryAuth::Anonymous,
            };
            (full_ref, auth)
        };

        let artifact = acquirer
            .acquire_rootfs_bundle_from_image_config(
                runner,
                ArtifactAcquireRequest::ExternalImage { image: full_ref },
                auth,
            )
            .await?;

        self.deploy(DeploymentPlan {
            service: service.clone(),
            artifact,
        })
        .await
    }

    pub async fn deploy_git_source(
        &self,
        service: &ServiceConfig,
        acquirer: &ArtifactAcquirer,
        runner: &dyn CommandRunner,
    ) -> Result<Deployment, DeployError> {
        let ServiceSource::Git(source) = &service.source else {
            return Err(DeployError::UnsupportedGitSource);
        };
        let artifact = acquirer
            .acquire_rootfs_bundle_from_image_config(
                runner,
                ArtifactAcquireRequest::Git {
                    repo_url: source.repo_url.clone(),
                    git_ref: source.git_ref.clone(),
                    dockerfile_path: source.dockerfile_path.clone(),
                    context_path: source.context_path.clone(),
                },
                RegistryAuth::Anonymous,
            )
            .await?;

        self.deploy(DeploymentPlan {
            service: service.clone(),
            artifact,
        })
        .await
    }

    pub async fn stop_service(&self, service: &ServiceConfig) -> Result<(), DeployError> {
        let promoted_deployment = self.repos.deployments.promoted_deployment(service.id)?;

        self.runtime
            .stop(&RuntimeInstanceId {
                service_id: service.id,
                service_name: service.name.clone(),
                replica_index: 0,
            })
            .await?;
        if let Some(routing) = &self.routing {
            let route_key = service.id.to_string();
            // Drop the workload replica from the proxy pool, then rebuild and
            // swap the route table from the trimmed snapshot.
            routing
                .ingress
                .remove_replica(&route_key, DEPLOY_REPLICA_ID)
                .await;
            let table = {
                let mut routes = routing
                    .routes
                    .lock()
                    .map_err(|_| DeployError::RoutesLockPoisoned)?;
                routes.remove(&route_key);
                route_table_from_snapshot(&routes)?
            };
            routing.ingress.swap_routes(table);
        }

        if let Some(deployment_id) = promoted_deployment {
            self.repos
                .deployments
                .update_deployment_status(deployment_id, DeploymentStatus::Stopped)?;
            self.repos
                .deployments
                .clear_promoted_deployment(service.id)?;
        }
        Ok(())
    }

    async fn write_routing_config(
        &self,
        service: &ServiceConfig,
        socket_path: &std::path::Path,
    ) -> Result<(), DeployError> {
        let Some(routing) = &self.routing else {
            return Ok(());
        };
        // Key replica/route state by service_id, not service.name — names are only
        // unique within a project, so two projects' same-named services would
        // otherwise share runtime/ingress state (F-3).
        let route_key = service.id.to_string();

        // Register the workload's Denia-owned Unix socket as the service's
        // (single) promoted replica and mark it healthy so the Pingora proxy can
        // dial it directly (no loopback bridge, ADR-020).
        routing
            .ingress
            .add_replica(&route_key, DEPLOY_REPLICA_ID, socket_path.to_path_buf())
            .await;
        routing
            .ingress
            .set_replica_healthy(&route_key, DEPLOY_REPLICA_ID, true)
            .await;

        let hostnames = self.repos.domains.list_verified_hostnames(service.id)?;
        if hostnames.is_empty() {
            // No verified domains yet — the replica is registered but the service
            // has no host route. A future verify call rebuilds the table only if
            // an entry exists, so the operator should verify a domain before
            // deploy, or re-deploy after verifying.
            return Ok(());
        }
        let table = {
            let mut routes = routing
                .routes
                .lock()
                .map_err(|_| DeployError::RoutesLockPoisoned)?;
            routes.insert(
                route_key.clone(),
                RouteSpec {
                    route_key: format!("svc-{}", service.id),
                    service_name: service.name.clone(),
                    // Proxy pool lookup key — MUST equal the `add_replica` key
                    // above (`service.id.to_string()`) so the Pingora hot path
                    // resolves Host -> route.service_id -> pool hit (C1).
                    service_id: route_key,
                    domains: hostnames,
                    tls: service.tls_enabled,
                },
            );
            route_table_from_snapshot(&routes)?
        };
        // Single control-plane writer: whole-table last-writer-wins swap (A8).
        routing.ingress.swap_routes(table);
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

#[cfg(test)]
mod async_tests {
    use super::*;
    use crate::domain::{
        ExternalImageSource, HealthCheck, ResourceLimits, ServiceConfig, ServiceSource,
    };
    use crate::health::FakeHealthChecker;
    use crate::repo::sqlite::{
        SqliteDeploymentRepo, SqliteDomainRepo, SqliteProjectRepo, SqliteRegistryRepo,
    };
    use crate::runtime::FakeRuntime;
    use crate::state::SqliteStore;

    fn build_repos(store: &SqliteStore) -> DeploymentRepos {
        let pool = store.pool();
        DeploymentRepos {
            deployments: SqliteDeploymentRepo::new(pool.clone()),
            projects: SqliteProjectRepo::new(pool.clone()),
            registries: SqliteRegistryRepo::new(pool.clone()),
            domains: SqliteDomainRepo::new(pool),
        }
    }

    fn coord_for_pending() -> (
        SqliteStore,
        DeploymentCoordinator<FakeRuntime, FakeHealthChecker>,
        ServiceConfig,
        DeploymentRequest,
    ) {
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let runtime = FakeRuntime::default();
        let health = FakeHealthChecker::healthy();
        let coordinator = DeploymentCoordinator::new(build_repos(&store), runtime, health);

        let project_id = store.default_project_id().expect("default project");
        let service = store
            .put_service(
                ServiceConfig::new(
                    project_id,
                    "web",
                    vec!["web.example.test".to_string()],
                    ServiceSource::ExternalImage(ExternalImageSource {
                        image: "ghcr.io/acme/web:latest".to_string(),
                        credential: None,
                        registry_id: None,
                        image_ref: None,
                    }),
                    3000,
                    HealthCheck::new("/ready", 5),
                    Some(ResourceLimits::default()),
                    vec![],
                )
                .expect("service"),
            )
            .expect("stored service");

        let request = DeploymentRequest::external_image(service.id, "ghcr.io/acme/web:latest");
        (store, coordinator, service, request)
    }

    #[tokio::test]
    async fn create_pending_persists_row_in_pending_status() {
        let (store, coord, svc, request) = coord_for_pending();
        let d = coord
            .create_pending(&svc, request.clone())
            .await
            .expect("create_pending");
        assert_eq!(d.status, DeploymentStatus::Pending);

        let row = store
            .list_deployments(svc.id)
            .expect("list deployments")
            .into_iter()
            .find(|d2| d2.id == d.id)
            .expect("row exists");
        assert_eq!(row.status, DeploymentStatus::Pending);
    }

    #[tokio::test]
    #[ignore = "filled in by Task 4 — acquire_artifact_for_run is unimplemented"]
    async fn run_transitions_pending_building_starting_healthy() {
        unimplemented!()
    }
}
