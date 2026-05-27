use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::artifacts::acquirer::{ArtifactAcquireRequest, ArtifactAcquirer};
use crate::artifacts::{ArtifactRecord, ArtifactSource};
use crate::bridge::{BridgeAllocator, BridgeManager};
use crate::command::CommandRunner;
use crate::deploy::error::DeployError;
use crate::deploy::routes::{SharedRoutes, default_ingress_options};
use crate::domain::ServiceSource;
use crate::domain::{
    Deployment, DeploymentRequest, DeploymentStatus, RuntimeInstanceId, RuntimeStartRequest,
    ServiceConfig,
};
use crate::health::HealthChecker;
use crate::oci::RegistryAuth;
use crate::repo::RepoError;
use crate::repo::sqlite::{
    SqliteDeploymentRepo, SqliteDomainRepo, SqliteProjectRepo, SqliteRegistryRepo,
};
use crate::runtime::Runtime;
use crate::traefik::{IngressRenderOptions, RouteSpec, render_file_provider_config};

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
    bridge: Arc<Mutex<BridgeAllocator>>,
    routes: SharedRoutes,
    manager: Arc<dyn BridgeManager>,
    traefik_config_path: PathBuf,
    ingress_options: IngressRenderOptions,
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
        bridge: BridgeAllocator,
        manager: Arc<dyn BridgeManager>,
        traefik_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_shared_routing(
            repos,
            runtime,
            health,
            Arc::new(Mutex::new(bridge)),
            manager,
            traefik_config_path,
            Arc::new(Mutex::new(BTreeMap::new())),
            default_ingress_options(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_shared_routing(
        repos: DeploymentRepos,
        runtime: R,
        health: H,
        bridge: Arc<Mutex<BridgeAllocator>>,
        manager: Arc<dyn BridgeManager>,
        traefik_config_path: impl Into<PathBuf>,
        routes: SharedRoutes,
        ingress_options: IngressRenderOptions,
    ) -> Self {
        Self {
            repos,
            runtime,
            health,
            routing: Some(RoutingState {
                bridge,
                routes,
                manager,
                traefik_config_path: traefik_config_path.into(),
                ingress_options,
            }),
        }
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
                        .decrypt(runner, sops_binary, secret_ref)
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
                        .decrypt(runner, sops_binary, secret_ref)
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
                service_name: service.name.clone(),
                replica_index: 0,
            })
            .await?;
        if let Some(routing) = &self.routing {
            routing.manager.deactivate(&service.name).await?;
            let yaml = {
                let mut routes = routing
                    .routes
                    .lock()
                    .map_err(|_| DeployError::BridgeLockPoisoned)?;
                routes.remove(&service.name);
                render_file_provider_config(
                    &routes.values().cloned().collect::<Vec<_>>(),
                    &routing.ingress_options,
                )?
            };
            std::fs::write(&routing.traefik_config_path, yaml)?;
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
        let bridge_target = routing
            .bridge
            .lock()
            .map_err(|_| DeployError::BridgeLockPoisoned)?
            .assign(&service.name, socket_path.to_path_buf())
            .ok_or(DeployError::BridgePortExhausted)?;
        routing.manager.activate(bridge_target.clone()).await?;

        let hostnames = self.repos.domains.list_verified_hostnames(service.id)?;
        if hostnames.is_empty() {
            // No verified domains yet — bridge is allocated but Traefik is not told
            // to route this service. A future verify call will not retroactively add
            // the route either (the routes map has no entry to update); the operator
            // can verify a domain before deploy, or re-deploy after verifying.
            return Ok(());
        }
        let mut routes = routing
            .routes
            .lock()
            .map_err(|_| DeployError::BridgeLockPoisoned)?;
        routes.insert(
            service.name.clone(),
            RouteSpec {
                route_key: format!("svc-{}", service.id),
                service_name: service.name.clone(),
                domains: hostnames,
                bridge_port: bridge_target.port,
                tls: service.tls_enabled,
            },
        );
        let yaml = render_file_provider_config(
            &routes.values().cloned().collect::<Vec<_>>(),
            &routing.ingress_options,
        )?;
        std::fs::write(&routing.traefik_config_path, yaml)?;
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
