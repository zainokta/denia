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
/// PLAIN (non-autoscaled) service. The deploy coordinator manages one promoted
/// replica per plain service; autoscaled services are owned by the controller,
/// which fans out replicas under its own (UUIDv7) ids and is NEVER given a
/// `DEPLOY_REPLICA_ID` endpoint (ADR-028). A fixed nil-derived id keeps this
/// endpoint addressable and replaceable on re-deploy.
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

/// Dependencies the async deploy task needs that the coordinator does not own.
///
/// These are injected per-run (rather than per-coordinator) so the API handler
/// can clone the acquirer/runner/secret store into the spawned `tokio::spawn`
/// task without widening the coordinator's constructor.
pub struct RunDeps<'a> {
    pub acquirer: &'a ArtifactAcquirer,
    pub runner: &'a dyn CommandRunner,
    pub secret_store: &'a crate::secrets::SopsSecretStore,
    pub sops_binary: &'a std::path::Path,
    /// Age private-key file passed to `sops` as `SOPS_AGE_KEY_FILE` when
    /// decrypting registry credentials. See `secrets::SopsSecretStore::decrypt`.
    pub age_key_file: &'a std::path::Path,
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
    /// Healthy, emitting per-phase log lines via `log`, using the supplied
    /// `deps` to resolve auth and acquire the artifact. On failure, writes an
    /// `ERROR` log line and transitions the row to `Failed`.
    pub async fn run_with_deps(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
        deps: RunDeps<'_>,
    ) -> Result<(), DeployError> {
        let res = self
            .run_inner_with_deps(deployment_id, service, request, log, deps)
            .await;
        if let Err(ref e) = res {
            let _ = log.write("ERROR", &format!("{e:?}"));
            let _ = self
                .repos
                .deployments
                .update_deployment_status(deployment_id, DeploymentStatus::Failed);
        }
        res
    }

    async fn run_inner_with_deps(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
        deps: RunDeps<'_>,
    ) -> Result<(), DeployError> {
        log.write("START", &format!("deployment_id={deployment_id}"))
            .ok();

        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Building)?;
        log.write("BUILDING", "resolving auth + acquiring artifact")
            .ok();

        let artifact = match &request {
            DeploymentRequest::ExternalImage { .. } => {
                let ServiceSource::ExternalImage(source) = &service.source else {
                    return Err(DeployError::UnsupportedServiceSource);
                };
                let (full_ref, auth) = resolve_external_auth(
                    &self.repos,
                    source,
                    service.project_id,
                    deps.secret_store,
                    deps.runner,
                    deps.sops_binary,
                    deps.age_key_file,
                )
                .await?;
                deps.acquirer
                    .acquire_rootfs_bundle_from_image_config(
                        deps.runner,
                        ArtifactAcquireRequest::ExternalImage { image: full_ref },
                        auth,
                    )
                    .await?
            }
            DeploymentRequest::Git { .. } => {
                let ServiceSource::Git(source) = &service.source else {
                    return Err(DeployError::UnsupportedGitSource);
                };
                deps.acquirer
                    .acquire_rootfs_bundle_from_image_config(
                        deps.runner,
                        ArtifactAcquireRequest::Git {
                            repo_url: source.repo_url.clone(),
                            git_ref: source.git_ref.clone(),
                            dockerfile_path: source.dockerfile_path.clone(),
                            context_path: source.context_path.clone(),
                        },
                        RegistryAuth::Anonymous,
                    )
                    .await?
            }
            DeploymentRequest::Upload {
                upload_id,
                dockerfile_path,
                context_path,
                ..
            } => {
                deps.acquirer
                    .acquire_rootfs_bundle_from_image_config(
                        deps.runner,
                        ArtifactAcquireRequest::Upload {
                            upload_id: upload_id.clone(),
                            dockerfile_path: dockerfile_path.clone(),
                            context_path: context_path.clone(),
                        },
                        RegistryAuth::Anonymous,
                    )
                    .await?
            }
        };
        log.write("OCI_PULL", "done").ok();
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

    /// Promote + wire-up portion of the deploy pipeline. Mirrors the body of
    /// `deploy()` but uses the supplied `deployment_id` (no `create_deployment`
    /// here) and emits log lines for the SSE viewer.
    ///
    /// Plain services: runtime-start `replica_index 0` → healthcheck → promote →
    /// `add_deploy_replica` → `write_route_table`. Autoscaled services hand
    /// replica ownership to the controller (ADR-028): promote + `write_route_table`
    /// only (no workload start, no `DEPLOY_REPLICA_ID`); the API layer then calls
    /// `Controller::reconcile_service`.
    async fn finalize(
        &self,
        deployment_id: Uuid,
        service: &ServiceConfig,
        artifact: ArtifactRecord,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<(), DeployError> {
        if service.autoscale.is_some() {
            // Autoscaled service: the autoscaler owns replica launch/teardown
            // (ADR-028). The deploy path must NOT start a DEPLOY_REPLICA_ID
            // workload or register an ingress replica — that would shadow the
            // controller (its registry would stay empty, `/v1/workloads` would
            // report 0, and scale-to-zero / cold-start would never engage). We
            // only promote and write the route table; the API layer then calls
            // `Controller::reconcile_service`, which launches `min` replicas
            // (each health-gated) or none for min==0 (woken by the activator).
            log.write(
                "AUTOSCALE_HANDOFF",
                "autoscaled service: controller owns replicas",
            )
            .ok();
            self.repos
                .deployments
                .promote_deployment(service.id, deployment_id)?;
            self.write_route_table(service).await?;
            return Ok(());
        }

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
        self.add_deploy_replica(service, &runtime_status.socket_path)
            .await?;
        self.write_route_table(service).await?;
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

        if plan.service.autoscale.is_some() {
            // Autoscaled service: hand replica ownership to the controller
            // (ADR-028). Persist + promote the deployment and write the route
            // table, but do not start a DEPLOY_REPLICA_ID workload. The caller
            // is responsible for invoking `Controller::reconcile_service`.
            self.repos
                .deployments
                .promote_deployment(plan.service.id, deployment.id)?;
            self.write_route_table(&plan.service).await?;
            self.repos
                .deployments
                .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
            deployment.status = DeploymentStatus::Healthy;
            return Ok(deployment);
        }

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
        self.add_deploy_replica(&plan.service, &runtime_status.socket_path)
            .await?;
        self.write_route_table(&plan.service).await?;
        self.repos
            .deployments
            .update_deployment_status(deployment.id, DeploymentStatus::Healthy)?;
        deployment.status = DeploymentStatus::Healthy;
        Ok(deployment)
    }

    /// Thin wrapper retained for tests in `tests/backend_contract.rs` that
    /// drive the external-image deploy pipeline directly. The synchronous,
    /// inline pipeline now goes through `create_pending` + `run_with_deps` so
    /// auth resolution + artifact acquisition share the same code path as the
    /// async API handler.
    ///
    /// Writes log lines to `<temp>/denia-test-logs/<deployment_id>.log`; the
    /// real API handler uses `<config.log_dir>/deployments/<id>.log`.
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
        let request = DeploymentRequest::ExternalImage {
            service_id: service.id,
            image: source.image.clone(),
        };
        let deployment = self.create_pending(service, request.clone()).await?;
        let log = test_log_writer(deployment.id)?;
        let deps = RunDeps {
            acquirer,
            runner,
            secret_store,
            sops_binary,
            // Test helper: callers use FakeCommandRunner, which ignores env.
            age_key_file: std::path::Path::new("/nonexistent-age-key"),
        };
        self.run_with_deps(deployment.id, service.clone(), request, &log, deps)
            .await?;
        let updated = self
            .repos
            .deployments
            .list_deployments(service.id)?
            .into_iter()
            .find(|d| d.id == deployment.id)
            .unwrap_or(deployment);
        Ok(updated)
    }

    /// Thin wrapper retained for parity with `deploy_external_image_source`.
    /// No existing test calls it directly today, but the API handler used to
    /// dispatch through it. Routed through the same async pipeline so behavior
    /// stays aligned.
    pub async fn deploy_git_source(
        &self,
        service: &ServiceConfig,
        acquirer: &ArtifactAcquirer,
        runner: &dyn CommandRunner,
    ) -> Result<Deployment, DeployError> {
        let ServiceSource::Git(source) = &service.source else {
            return Err(DeployError::UnsupportedGitSource);
        };
        let request = DeploymentRequest::Git {
            service_id: service.id,
            repo_url: source.repo_url.clone(),
            git_ref: source.git_ref.clone(),
        };
        let deployment = self.create_pending(service, request.clone()).await?;
        let log = test_log_writer(deployment.id)?;
        // Git path does not consult registry credentials today; pass an empty
        // SopsSecretStore + sops binary placeholder. `resolve_external_auth`
        // is not invoked for the Git branch in `run_inner_with_deps`.
        let secret_store = crate::secrets::SopsSecretStore::new(std::env::temp_dir());
        let sops_binary = std::path::PathBuf::from("sops");
        let deps = RunDeps {
            acquirer,
            runner,
            secret_store: &secret_store,
            sops_binary: sops_binary.as_path(),
            // Git path never calls resolve_external_auth, so this is unused.
            age_key_file: std::path::Path::new("/nonexistent-age-key"),
        };
        self.run_with_deps(deployment.id, service.clone(), request, &log, deps)
            .await?;
        let updated = self
            .repos
            .deployments
            .list_deployments(service.id)?
            .into_iter()
            .find(|d| d.id == deployment.id)
            .unwrap_or(deployment);
        Ok(updated)
    }

    /// Restart the currently-promoted deployment of `service` from its already
    /// acquired artifact, re-wiring ingress at `replica_index 0`. Used by boot
    /// autostart for plain (non-autoscaled) services: a service whose promoted
    /// deployment is still set "should be running" (explicit stop clears the
    /// promoted row, so a stopped service is skipped here).
    ///
    /// No-op (`Ok`) when there is no promoted deployment or its artifact is
    /// missing (e.g. GC'd) — the caller logs and moves on. Re-promotion via
    /// `finalize` is idempotent.
    pub async fn restart_promoted(
        &self,
        service: &ServiceConfig,
        log: &crate::deploy::log::DeploymentLogWriter,
    ) -> Result<(), DeployError> {
        let Some(deployment_id) = self.repos.deployments.promoted_deployment(service.id)? else {
            return Ok(());
        };
        let Some(artifact) = self
            .repos
            .deployments
            .get_deployment_artifact(deployment_id)?
        else {
            let _ = log.write(
                "AUTOSTART",
                "promoted deployment has no artifact (GC'd); skipping",
            );
            return Ok(());
        };
        self.finalize(deployment_id, service, artifact, log).await
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

    /// Stop path for an autoscaled service. The controller has already drained
    /// every replica (released the ledger, removed the ingress + registry
    /// entries), so this only tears down the route + deployment state. Unlike
    /// `stop_service` it does NOT `runtime.stop(replica 0)` or
    /// `remove_replica(DEPLOY_REPLICA_ID)` — an autoscaled service never has a
    /// `DEPLOY_REPLICA_ID` endpoint (ADR-028). Clearing the promoted row is the
    /// durable "should not be running" signal that keeps the autoscaler from
    /// relaunching on the next tick or boot.
    pub async fn stop_service_routes_only(
        &self,
        service: &ServiceConfig,
    ) -> Result<(), DeployError> {
        let promoted_deployment = self.repos.deployments.promoted_deployment(service.id)?;

        if let Some(routing) = &self.routing {
            let route_key = service.id.to_string();
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

    /// Register the deployed workload's Denia-owned Unix socket as the service's
    /// single promoted replica (`DEPLOY_REPLICA_ID`) and mark it healthy so the
    /// Pingora proxy can dial it directly (no loopback bridge, ADR-020). Plain
    /// (non-autoscaled) services only — autoscaled services have their replicas
    /// registered by the controller (ADR-028).
    async fn add_deploy_replica(
        &self,
        service: &ServiceConfig,
        socket_path: &std::path::Path,
    ) -> Result<(), DeployError> {
        let Some(routing) = &self.routing else {
            return Ok(());
        };
        // Key replica state by service_id, not service.name — names are only
        // unique within a project, so two projects' same-named services would
        // otherwise share runtime/ingress state (F-3).
        let route_key = service.id.to_string();
        routing
            .ingress
            .add_replica(&route_key, DEPLOY_REPLICA_ID, socket_path.to_path_buf())
            .await;
        routing
            .ingress
            .set_replica_healthy(&route_key, DEPLOY_REPLICA_ID, true)
            .await;
        Ok(())
    }

    /// Rebuild and swap the ingress route table (verified domains -> service_id)
    /// from the current snapshot. Runs for BOTH plain and autoscaled services so
    /// Host/SNI routing and the scale-to-zero activator can resolve the service
    /// even when it has zero live replicas.
    async fn write_route_table(&self, service: &ServiceConfig) -> Result<(), DeployError> {
        let Some(routing) = &self.routing else {
            return Ok(());
        };
        // Key route state by service_id, not service.name (F-3).
        let route_key = service.id.to_string();

        let hostnames = self.repos.domains.list_verified_hostnames(service.id)?;
        if hostnames.is_empty() {
            // No verified domains yet — no host route. A future verify call
            // rebuilds the table only if an entry exists, so the operator should
            // verify a domain before deploy, or re-deploy after verifying.
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
                    // Proxy pool lookup key — MUST equal the replica pool key
                    // (`service.id.to_string()`) so the Pingora hot path resolves
                    // Host -> route.service_id -> pool hit (C1).
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

/// Resolve `(full_image_ref, auth)` for an external-image deploy. Authenticated
/// images must reference a registry row, which carries `auth_kind` plus an
/// optional encrypted credential ref; legacy inline credentials fail closed.
async fn resolve_external_auth(
    repos: &DeploymentRepos,
    source: &crate::domain::ExternalImageSource,
    _project_id: Uuid,
    secret_store: &crate::secrets::SopsSecretStore,
    runner: &dyn CommandRunner,
    sops_binary: &std::path::Path,
    age_key_file: &std::path::Path,
) -> Result<(String, RegistryAuth), DeployError> {
    if source.credential.is_some() {
        return Err(DeployError::UnsupportedServiceSource);
    }
    if let Some(registry_id) = source.registry_id {
        let registry = repos
            .registries
            .registry(registry_id)?
            .ok_or(DeployError::RegistryNotFound)?;
        let payload = match &registry.credential_ref {
            Some(secret_ref) => Some(
                secret_store
                    .decrypt(
                        runner,
                        sops_binary,
                        age_key_file,
                        registry.project_id,
                        secret_ref,
                    )
                    .await?,
            ),
            None => None,
        };
        let auth =
            crate::oci::credentials::resolve_registry_auth(registry.auth_kind, payload.as_ref())
                .map_err(DeployError::RegistryAuthResolution)?;
        let (full_ref, _) = source
            .resolve_ref(&registry.endpoint)
            .map_err(|_| DeployError::UnsupportedServiceSource)?;
        Ok((full_ref, auth))
    } else {
        let (full_ref, _) = source
            .resolve_ref("")
            .map_err(|_| DeployError::UnsupportedServiceSource)?;
        Ok((full_ref, RegistryAuth::Anonymous))
    }
}

/// Build a `DeploymentLogWriter` under a shared per-process temp directory so
/// the thin `deploy_*_source` wrappers (test entry points) write somewhere
/// stable without needing an `AppConfig.log_dir` injection.
fn test_log_writer(
    deployment_id: Uuid,
) -> Result<crate::deploy::log::DeploymentLogWriter, DeployError> {
    let dir = std::env::temp_dir().join("denia-test-logs");
    std::fs::create_dir_all(&dir).ok();
    let writer = crate::deploy::log::DeploymentLogWriter::create(&dir, deployment_id)?;
    Ok(writer)
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
        ArtifactSource::UploadedContext {
            upload_id,
            dockerfile_path,
            context_path,
        } => DeploymentRequest::Upload {
            service_id: service.id,
            upload_id: upload_id.clone(),
            dockerfile_path: dockerfile_path.clone(),
            context_path: context_path.clone(),
        },
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use crate::domain::{
        AutoscalePolicy, ExternalImageSource, HealthCheck, ResourceLimits, ServiceConfig,
        ServiceSource,
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

    /// Build a coordinator wired to a clonable `FakeRuntime` (so the test can
    /// inspect `started_requests` afterwards) plus a stored service.
    fn coord_with_runtime() -> (
        SqliteStore,
        DeploymentCoordinator<FakeRuntime, FakeHealthChecker>,
        FakeRuntime,
        ServiceConfig,
    ) {
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let runtime = FakeRuntime::default();
        let coordinator = DeploymentCoordinator::new(
            build_repos(&store),
            runtime.clone(),
            FakeHealthChecker::healthy(),
        );
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
        (store, coordinator, runtime, service)
    }

    #[tokio::test]
    async fn restart_promoted_relaunches_from_existing_artifact() {
        use crate::artifacts::{ArtifactKind, ArtifactRecord};
        let (store, coordinator, runtime, service) = coord_with_runtime();
        let repos = build_repos(&store);

        let deployment = repos
            .deployments
            .create_deployment(DeploymentRequest::external_image(
                service.id,
                "ghcr.io/acme/web:latest",
            ))
            .expect("deployment");
        let artifact = ArtifactRecord::new(
            "sha256:deadbeef",
            ArtifactKind::OciImage,
            ArtifactSource::ExternalRegistry {
                image: "ghcr.io/acme/web:latest".to_string(),
            },
        )
        .expect("artifact");
        repos
            .deployments
            .put_artifact(artifact)
            .expect("put artifact");
        repos
            .deployments
            .set_deployment_artifact(deployment.id, "sha256:deadbeef")
            .expect("link artifact");
        repos
            .deployments
            .promote_deployment(service.id, deployment.id)
            .expect("promote");

        let log = test_log_writer(deployment.id).expect("log");
        coordinator
            .restart_promoted(&service, &log)
            .await
            .expect("restart");

        let started = runtime.started_requests();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].replica_index, 0);
        assert_eq!(started[0].service_id, service.id);
        assert_eq!(started[0].deployment_id, deployment.id);
        // Re-promote is idempotent: the promotion survives.
        assert_eq!(
            repos
                .deployments
                .promoted_deployment(service.id)
                .expect("promoted"),
            Some(deployment.id)
        );
    }

    #[tokio::test]
    async fn restart_promoted_no_promoted_is_noop() {
        let (_store, coordinator, runtime, service) = coord_with_runtime();
        // No promoted deployment for this service.
        let log = test_log_writer(uuid::Uuid::now_v7()).expect("log");
        coordinator
            .restart_promoted(&service, &log)
            .await
            .expect("noop ok");
        assert!(runtime.started_requests().is_empty());
    }

    #[tokio::test]
    async fn restart_promoted_skips_when_artifact_missing() {
        let (store, coordinator, runtime, service) = coord_with_runtime();
        let repos = build_repos(&store);
        // Promote a deployment WITHOUT linking an artifact (e.g. GC'd).
        let deployment = repos
            .deployments
            .create_deployment(DeploymentRequest::external_image(
                service.id,
                "ghcr.io/acme/web:latest",
            ))
            .expect("deployment");
        repos
            .deployments
            .promote_deployment(service.id, deployment.id)
            .expect("promote");

        let log = test_log_writer(deployment.id).expect("log");
        coordinator
            .restart_promoted(&service, &log)
            .await
            .expect("skip ok");
        assert!(runtime.started_requests().is_empty());
    }

    /// ADR-028: deploying an autoscaled service must NOT start a
    /// DEPLOY_REPLICA_ID workload or register an ingress replica — the
    /// controller owns replicas. The deploy only promotes + writes routes.
    #[tokio::test]
    async fn deploy_autoscaled_does_not_start_replica() {
        use crate::artifacts::{ArtifactKind, ArtifactRecord};
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let runtime = FakeRuntime::default();
        let ingress = std::sync::Arc::new(IngressState::default());
        let routes: SharedRoutes =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let coordinator = DeploymentCoordinator::new_with_shared_routing(
            build_repos(&store),
            runtime.clone(),
            FakeHealthChecker::healthy(),
            ingress.clone(),
            routes.clone(),
        );

        let project_id = store.default_project_id().expect("default project");
        let mut svc = ServiceConfig::new(
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
        .expect("service");
        svc.autoscale = Some(AutoscalePolicy {
            min_replicas: 0,
            max_replicas: 3,
            target_cpu_pct: 80,
            target_mem_pct: None,
            scale_down_cooldown_s: 300,
            idle_timeout_s: 600,
        });
        let svc = store.put_service(svc).expect("stored service");

        let artifact = ArtifactRecord::new(
            "sha256:deadbeef",
            ArtifactKind::OciImage,
            ArtifactSource::ExternalRegistry {
                image: "ghcr.io/acme/web:latest".to_string(),
            },
        )
        .expect("artifact");

        let deployment = coordinator
            .deploy(DeploymentPlan {
                service: svc.clone(),
                artifact,
            })
            .await
            .expect("deploy ok");

        // No DEPLOY_REPLICA_ID workload started; no ingress replica registered.
        assert!(runtime.started_requests().is_empty());
        assert_eq!(ingress.healthy_count(&svc.id.to_string()).await, 0);
        // But the deployment is promoted + Healthy.
        assert_eq!(deployment.status, DeploymentStatus::Healthy);
        assert_eq!(
            build_repos(&store)
                .deployments
                .promoted_deployment(svc.id)
                .expect("promoted"),
            Some(deployment.id)
        );
    }

    /// ADR-028: the autoscaled stop path tears down route + deployment state
    /// only — the controller has already drained the replicas, so this must NOT
    /// `runtime.stop()` a `DEPLOY_REPLICA_ID` workload.
    #[tokio::test]
    async fn stop_service_routes_only_clears_promoted_without_runtime_stop() {
        let (store, coordinator, runtime, service) = coord_with_runtime();
        let repos = build_repos(&store);
        let deployment = repos
            .deployments
            .create_deployment(DeploymentRequest::external_image(
                service.id,
                "ghcr.io/acme/web:latest",
            ))
            .expect("deployment");
        repos
            .deployments
            .promote_deployment(service.id, deployment.id)
            .expect("promote");

        coordinator
            .stop_service_routes_only(&service)
            .await
            .expect("stop routes-only");

        // No runtime.stop of replica 0 (the controller already drained).
        assert!(runtime.stopped_instances().is_empty());
        // Promoted row cleared + deployment Stopped.
        assert_eq!(
            repos
                .deployments
                .promoted_deployment(service.id)
                .expect("promoted"),
            None
        );
        let row = repos
            .deployments
            .list_deployments(service.id)
            .expect("list")
            .into_iter()
            .find(|d| d.id == deployment.id)
            .expect("row");
        assert_eq!(row.status, DeploymentStatus::Stopped);
    }
}
