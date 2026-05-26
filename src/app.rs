use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{
    access_log::{AccessEntry, AccessLogStore},
    artifacts::acquirer::ArtifactAcquirer,
    auth::{Principal, require_project_role, resolve_auth},
    bridge::{BridgeAllocator, BridgeManager, LoopbackBridgeSupervisor},
    command::{CommandRunner, TokioCommandRunner},
    config::AppConfig,
    deploy::{DeployError, DeploymentCoordinator, DeploymentRepos, SharedRoutes},
    domain::{
        ApiToken, Credential, CredentialKind, DeploymentRequest, DomainStatus, Job, JobRun,
        LoginResult, Me, PrincipalView, Project, ProjectMembership, Role, ServiceConfig,
        ServiceDomain,
    },
    health::{FakeHealthChecker, HealthChecker},
    logs::LogStore,
    metrics::{CgroupMetricsReader, MetricsError},
    node_metrics::{NodeMetricsError, NodeMetricsReader, NodeSnapshot},
    rate_limit::{LoginRateLimiter, rate_limit_login},
    repo::{
        CredentialRepo, DeploymentRepo, DomainRepo, JobRepo, ProjectRepo, RegistryRepo, RepoError,
        ServiceRepo, TokenRepo, UserRepo,
        sqlite::{
            SqliteCredentialRepo, SqliteDeploymentRepo, SqliteDomainRepo, SqliteJobRepo,
            SqliteProjectRepo, SqliteRegistryRepo, SqliteServiceRepo, SqliteTokenRepo,
            SqliteUserRepo,
        },
    },
    runtime::{LinuxRuntime, Runtime},
    secrets::SecretRef,
    state::SqliteStore,
    traefik::{IngressRenderOptions, RouteSpec},
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub services: Arc<dyn ServiceRepo>,
    pub domains: Arc<dyn DomainRepo>,
    pub registries: Arc<dyn RegistryRepo>,
    pub projects: Arc<dyn ProjectRepo>,
    pub users: Arc<dyn UserRepo>,
    pub deployments: Arc<dyn DeploymentRepo>,
    pub jobs: Arc<dyn JobRepo>,
    pub tokens: Arc<dyn TokenRepo>,
    pub credentials: Arc<dyn CredentialRepo>,
    runtime: Arc<dyn Runtime>,
    health: Arc<dyn HealthChecker>,
    command_runner: Arc<dyn CommandRunner>,
    bridge_allocator: Arc<Mutex<BridgeAllocator>>,
    bridge_manager: Arc<dyn BridgeManager>,
    pub routes: SharedRoutes,
    pub ingress_options: IngressRenderOptions,
    pub access_log: AccessLogStore,
    pub domain_verifier: Arc<dyn crate::verification::DomainVerifier>,
    pub verifying_domains: Arc<Mutex<std::collections::HashSet<uuid::Uuid>>>,
}

impl AppState {
    pub fn new(config: AppConfig, store: &SqliteStore) -> Self {
        let bridge_start_port = config.bridge_start_port;
        let runtime = Arc::new(
            LinuxRuntime::new_with_paths(
                config.runtime_dir.clone(),
                config.artifact_dir.clone(),
                config.cgroup_root.clone(),
            )
            .with_userns(config.userns_base, config.userns_size)
            .with_socket_proxy(config.socket_proxy_binary.clone())
            .with_log_dir(config.log_dir.clone()),
        );
        let access_log = AccessLogStore::new();
        let supervisor = LoopbackBridgeSupervisor::with_access_log(access_log.clone());
        Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime,
            FakeHealthChecker::healthy(),
            TokioCommandRunner,
            BridgeAllocator::new(bridge_start_port),
            supervisor,
            access_log,
        )
    }

    pub fn new_with_deploy_dependencies<R, H, C, B, M>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        bridge_allocator: B,
        bridge_manager: M,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
        B: Into<BridgeAllocator>,
        M: BridgeManager + 'static,
    {
        Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime,
            health,
            command_runner,
            bridge_allocator,
            bridge_manager,
            AccessLogStore::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_deploy_dependencies_and_log<R, H, C, B, M>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        bridge_allocator: B,
        bridge_manager: M,
        access_log: AccessLogStore,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
        B: Into<BridgeAllocator>,
        M: BridgeManager + 'static,
    {
        let ingress_options = IngressRenderOptions {
            acme_resolver: config.acme_resolver.clone(),
            control_domain: config.control_domain.clone(),
            control_tls: config.control_tls,
            control_backend_addr: format!("http://{}", config.bind_addr),
        };
        let pool = store.pool();
        Self {
            config,
            services: Arc::new(SqliteServiceRepo::new(pool.clone())),
            domains: Arc::new(SqliteDomainRepo::new(pool.clone())),
            registries: Arc::new(SqliteRegistryRepo::new(pool.clone())),
            projects: Arc::new(SqliteProjectRepo::new(pool.clone())),
            users: Arc::new(SqliteUserRepo::new(pool.clone())),
            deployments: Arc::new(SqliteDeploymentRepo::new(pool.clone())),
            jobs: Arc::new(SqliteJobRepo::new(pool.clone())),
            tokens: Arc::new(SqliteTokenRepo::new(pool.clone())),
            credentials: Arc::new(SqliteCredentialRepo::new(pool)),
            runtime: Arc::new(runtime),
            health: Arc::new(health),
            command_runner: Arc::new(command_runner),
            bridge_allocator: Arc::new(Mutex::new(bridge_allocator.into())),
            bridge_manager: Arc::new(bridge_manager),
            routes: Arc::new(Mutex::new(BTreeMap::new())),
            ingress_options,
            access_log,
            domain_verifier: Arc::new(crate::verification::HttpDomainVerifier::new()),
            verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub fn with_domain_verifier(
        mut self,
        verifier: Arc<dyn crate::verification::DomainVerifier>,
    ) -> Self {
        self.domain_verifier = verifier;
        self
    }

    /// Build a `DeploymentRepos` bundle from this state for handler-side
    /// coordinator construction.
    fn deployment_repos(&self) -> DeploymentRepos {
        DeploymentRepos {
            deployments: self.deployments.clone(),
            projects: self.projects.clone(),
            registries: self.registries.clone(),
            domains: self.domains.clone(),
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let rate_limiter = LoginRateLimiter::default();
    let login = Router::new()
        .route("/auth/login", post(login_handler))
        .route_layer(middleware::from_fn_with_state(
            rate_limiter,
            rate_limit_login,
        ));

    let auth_public = Router::new().merge(login);

    let auth_routes = Router::new()
        .route("/auth/logout", post(logout_handler))
        .route("/me", get(me_handler))
        .route("/users", get(list_users).post(create_user_handler))
        .route("/users/{user_id}", delete(delete_user_handler))
        .route(
            "/api-tokens",
            get(list_api_tokens_handler).post(create_api_token_handler),
        )
        .route("/api-tokens/{token_id}", delete(revoke_api_token_handler))
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{job_id}", get(get_job).delete(delete_job))
        .route("/jobs/{job_id}/run", post(run_job))
        .route("/jobs/{job_id}/runs", get(list_job_runs))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let protected = Router::new()
        .route("/credentials/git", post(put_credential))
        .route("/credentials/registry", post(put_credential))
        .route("/services", get(list_services).post(put_service))
        .route("/deployments", post(create_deployment))
        .route(
            "/services/{service_id}/deployments",
            get(list_service_deployments),
        )
        .route("/services/{service_id}/logs", get(service_logs))
        .route("/services/{service_id}/metrics", get(service_metrics))
        .route(
            "/services/{service_id}/domains",
            get(list_service_domains).post(create_service_domain),
        )
        .route(
            "/services/{service_id}/domains/{domain_id}",
            delete(delete_service_domain_handler),
        )
        .route(
            "/services/{service_id}/domains/{domain_id}/verify",
            post(verify_service_domain),
        )
        .route("/services/{service_id}/{action}", post(lifecycle_command))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).delete(delete_project),
        )
        .route(
            "/projects/{project_id}/members",
            get(list_project_members).post(add_project_member),
        )
        .route(
            "/projects/{project_id}/members/{user_id}",
            delete(remove_project_member),
        )
        .route(
            "/projects/{project_id}/registries",
            get(list_registries).post(create_registry),
        )
        .route(
            "/projects/{project_id}/registries/{registry_id}",
            get(get_registry)
                .patch(update_registry_handler)
                .delete(delete_registry_handler),
        )
        .route("/ingress/routes", get(list_ingress_routes))
        .route("/ingress/config", get(get_ingress_config))
        .route("/metrics/node", get(get_node_metrics))
        .route("/workloads", get(list_workloads))
        .route(
            "/services/{service_id}/requests",
            get(list_service_requests),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/.well-known/denia-challenge/{token}",
            get(challenge_handler),
        )
        .nest("/v1", auth_public.merge(auth_routes).merge(protected))
        .layer(middleware::from_fn(security_headers))
        .fallback(crate::web::static_handler)
        .with_state(state)
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn challenge_handler(
    State(state): State<AppState>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<axum::response::Response, ApiError> {
    match state.domains.get_service_domain_by_token(&token)? {
        Some(_) => Ok(([(header::CONTENT_TYPE, "text/plain")], token).into_response()),
        None => Err(ApiError::NotFound("challenge token not found".into())),
    }
}

fn ensure_role(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    min: Role,
) -> Result<(), ApiError> {
    if principal.is_super_admin {
        return Ok(());
    }
    let user_id = principal
        .user_id
        .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
    let role = state.users.role_for(user_id, project_id)?;
    require_project_role(principal, role, min).map_err(Into::into)
}

fn ensure_super_admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.is_super_admin {
        Ok(())
    } else {
        Err(ApiError::Forbidden("super admin required".to_string()))
    }
}

async fn list_services(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<ServiceConfig>>, ApiError> {
    let all = state.services.list_services()?;
    if principal.is_super_admin {
        return Ok(Json(all));
    }
    let user_id = principal
        .user_id
        .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    let allowed: std::collections::HashSet<uuid::Uuid> =
        memberships.into_iter().map(|m| m.project_id).collect();
    Ok(Json(
        all.into_iter()
            .filter(|s| allowed.contains(&s.project_id))
            .collect(),
    ))
}

async fn put_service(
    State(state): State<AppState>,
    principal: Principal,
    Json(service): Json<ServiceConfig>,
) -> Result<Json<ServiceConfig>, ApiError> {
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    if let crate::domain::ServiceSource::ExternalImage(src) = &service.source {
        src.validate()
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if let Some(registry_id) = src.registry_id {
            let registry = state
                .registries
                .registry(registry_id)?
                .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
            if registry.project_id != service.project_id {
                return Err(ApiError::NotFound("registry not found".into()));
            }
        }
    }
    Ok(Json(state.services.put_service(service)?))
}

async fn create_deployment(
    State(state): State<AppState>,
    principal: Principal,
    Json(request): Json<DeploymentRequest>,
) -> Result<Json<crate::domain::Deployment>, ApiError> {
    let Some(service) = state.services.get_service(request.service_id())? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    match request {
        DeploymentRequest::ExternalImage { .. } => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.deployment_repos(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
                state.routes.clone(),
                state.ingress_options.clone(),
            );
            let acquirer = ArtifactAcquirer::new(state.config.clone());
            let secret_store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
            Ok(Json(
                coordinator
                    .deploy_external_image_source(
                        &service,
                        &acquirer,
                        state.command_runner.as_ref(),
                        &secret_store,
                        state.config.sops_binary.as_path(),
                    )
                    .await?,
            ))
        }
        DeploymentRequest::Git { .. } => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.deployment_repos(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
                state.routes.clone(),
                state.ingress_options.clone(),
            );
            let acquirer = ArtifactAcquirer::new(state.config.clone());
            Ok(Json(
                coordinator
                    .deploy_git_source(&service, &acquirer, state.command_runner.as_ref())
                    .await?,
            ))
        }
    }
}

#[derive(Debug, Serialize)]
struct WorkloadView {
    service_id: uuid::Uuid,
    service_name: String,
    project_id: uuid::Uuid,
    deployment_id: Option<uuid::Uuid>,
    status: Option<crate::domain::DeploymentStatus>,
    cpu_usage_usec: Option<u64>,
    memory_current_bytes: Option<u64>,
}

async fn get_node_metrics(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<NodeSnapshot>, ApiError> {
    ensure_super_admin(&principal)?;
    let reader = NodeMetricsReader::new(state.config.node_disk_path.clone());
    Ok(Json(reader.read()?))
}

async fn list_workloads(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<WorkloadView>>, ApiError> {
    let services = state.services.list_services()?;
    let allowed = if principal.is_super_admin {
        None
    } else {
        let user_id = principal
            .user_id
            .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
        let memberships = state.users.list_memberships_for_user(user_id)?;
        Some(
            memberships
                .into_iter()
                .map(|m| m.project_id)
                .collect::<std::collections::HashSet<_>>(),
        )
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    let mut workloads = Vec::new();
    for service in services {
        if let Some(ref a) = allowed
            && !a.contains(&service.project_id)
        {
            continue;
        }
        let deployment_id = state.deployments.promoted_deployment(service.id)?;
        let (cpu, mem) = match deployment_id {
            Some(d) => match reader.read_by_id(&service.name, service.id, d) {
                Ok(snap) => (Some(snap.cpu_usage_usec), Some(snap.memory_current_bytes)),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        let status = match deployment_id {
            Some(d) => state
                .deployments
                .list_deployments(service.id)?
                .into_iter()
                .find(|dep| dep.id == d)
                .map(|dep| dep.status),
            None => None,
        };
        workloads.push(WorkloadView {
            service_id: service.id,
            service_name: service.name.clone(),
            project_id: service.project_id,
            deployment_id,
            status,
            cpu_usage_usec: cpu,
            memory_current_bytes: mem,
        });
    }
    Ok(Json(workloads))
}

async fn list_service_requests(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<AccessEntry>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    Ok(Json(state.access_log.recent(&service.name)))
}

async fn list_ingress_routes(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<RouteSpec>>, ApiError> {
    ensure_super_admin(&principal)?;
    let routes = state
        .routes
        .lock()
        .map_err(|_| ApiError::Conflict("routes lock poisoned".to_string()))?;
    Ok(Json(routes.values().cloned().collect()))
}

async fn get_ingress_config(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    ensure_super_admin(&principal)?;
    let snapshot: Vec<RouteSpec> = {
        let routes = state
            .routes
            .lock()
            .map_err(|_| ApiError::Conflict("routes lock poisoned".to_string()))?;
        routes.values().cloned().collect()
    };
    let body = crate::traefik::render_file_provider_config(&snapshot, &state.ingress_options)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "text/yaml")], body).into_response())
}

async fn list_projects(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<Project>>, ApiError> {
    let all = state.projects.list_projects()?;
    if principal.is_super_admin {
        return Ok(Json(all));
    }
    let user_id = principal
        .user_id
        .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    let allowed: std::collections::HashSet<uuid::Uuid> =
        memberships.into_iter().map(|m| m.project_id).collect();
    Ok(Json(
        all.into_iter()
            .filter(|p| allowed.contains(&p.id))
            .collect(),
    ))
}

async fn get_project(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Project>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    let project = state
        .projects
        .get_project(project_id)?
        .ok_or_else(|| ApiError::NotFound("project not found".to_string()))?;
    Ok(Json(project))
}

async fn create_project(
    State(state): State<AppState>,
    principal: Principal,
    Json(project): Json<Project>,
) -> Result<Json<Project>, ApiError> {
    ensure_super_admin(&principal)?;
    Ok(Json(state.projects.put_project(project)?))
}

async fn delete_project(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    state.projects.delete_project(project_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

#[derive(Debug, Deserialize)]
struct AddMemberRequest {
    user_id: uuid::Uuid,
    role: Role,
}

#[derive(Deserialize)]
struct CreateDomainBody {
    hostname: String,
}

async fn list_project_members(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<ProjectMembership>>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    Ok(Json(state.users.list_members(project_id)?))
}

async fn add_project_member(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
    Json(input): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<ProjectMembership>), ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    state
        .users
        .set_membership(input.user_id, project_id, input.role)?;
    Ok((
        StatusCode::CREATED,
        Json(ProjectMembership {
            user_id: input.user_id,
            project_id,
            role: input.role,
        }),
    ))
}

async fn remove_project_member(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, user_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    state.users.remove_membership(user_id, project_id)?;
    Ok(Json(serde_json::json!({"removed": true})))
}

#[derive(Debug, Deserialize)]
struct RegistryInput {
    name: String,
    endpoint: String,
    auth_kind: crate::domain::RegistryAuthKind,
    #[serde(default)]
    secret_ref: Option<String>,
}

async fn list_registries(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::domain::Registry>>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    Ok(Json(state.registries.registries_for_project(project_id)?))
}

async fn create_registry(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
    Json(input): Json<RegistryInput>,
) -> Result<(StatusCode, Json<crate::domain::Registry>), ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let credential_ref = input
        .secret_ref
        .map(SecretRef::parse)
        .transpose()
        .map_err(ApiError::InvalidSecretRef)?;
    let registry = crate::domain::Registry::new(
        project_id,
        input.name,
        input.endpoint,
        input.auth_kind,
        credential_ref,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state.registries.create_registry(&registry)?;
    Ok((StatusCode::CREATED, Json(registry)))
}

async fn get_registry(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<crate::domain::Registry>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let registry = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if registry.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    Ok(Json(registry))
}

async fn update_registry_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
    Json(input): Json<RegistryInput>,
) -> Result<Json<crate::domain::Registry>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let existing = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if existing.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    let credential_ref = input
        .secret_ref
        .map(SecretRef::parse)
        .transpose()
        .map_err(ApiError::InvalidSecretRef)?;
    let mut updated = crate::domain::Registry::new(
        project_id,
        input.name,
        input.endpoint,
        input.auth_kind,
        credential_ref,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    updated.id = registry_id;
    state.registries.update_registry(&updated)?;
    Ok(Json(updated))
}

async fn delete_registry_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let registry = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if registry.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    state.registries.delete_registry(registry_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

async fn put_credential(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CredentialInput>,
) -> Result<Json<Credential>, ApiError> {
    ensure_super_admin(&principal)?;
    let secret_ref = SecretRef::parse(input.secret_ref).map_err(ApiError::InvalidSecretRef)?;
    Ok(Json(
        state
            .credentials
            .put_credential(input.name, input.kind, secret_ref)?,
    ))
}

async fn list_service_deployments(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::domain::Deployment>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    Ok(Json(state.deployments.list_deployments(service_id)?))
}

async fn create_service_domain(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
    Json(body): Json<CreateDomainBody>,
) -> Result<(StatusCode, Json<ServiceDomain>), ApiError> {
    let svc = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let hostname = crate::verification::validate_hostname(&body.hostname)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let token = crate::verification::generate_token();
    let now = chrono::Utc::now();
    let d = ServiceDomain {
        id: uuid::Uuid::now_v7(),
        service_id,
        hostname,
        status: DomainStatus::Pending,
        challenge_token: token,
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: now,
    };
    state.domains.put_service_domain(&d).map_err(|e| match e {
        RepoError::Sqlite(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            ApiError::Conflict("hostname already in use".into())
        }
        other => ApiError::Repo(other),
    })?;
    Ok((StatusCode::CREATED, Json(d)))
}

async fn list_service_domains(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<ServiceDomain>>, ApiError> {
    let svc = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Viewer)?;
    Ok(Json(
        state.domains.list_service_domains_by_service(service_id)?,
    ))
}

async fn verify_service_domain(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, domain_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<ServiceDomain>, ApiError> {
    let svc = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let d = state
        .domains
        .get_service_domain(domain_id)?
        .ok_or_else(|| ApiError::NotFound("domain not found".into()))?;
    if d.service_id != service_id {
        return Err(ApiError::NotFound("domain not found".into()));
    }
    if d.status == DomainStatus::Verified {
        return Ok(Json(d));
    }

    {
        let mut guard = state
            .verifying_domains
            .lock()
            .map_err(|_| ApiError::Conflict("verifier lock poisoned".into()))?;
        if !guard.insert(d.id) {
            return Err(ApiError::Conflict(
                "domain verification already in progress".into(),
            ));
        }
    }

    let result = state
        .domain_verifier
        .verify(&d.hostname, &d.challenge_token)
        .await;

    {
        let mut guard = state.verifying_domains.lock().unwrap();
        guard.remove(&d.id);
    }

    let updated = match result {
        Ok(()) => {
            state.domains.update_service_domain_status(
                d.id,
                DomainStatus::Verified,
                Some(chrono::Utc::now()),
                None,
            )?;
            crate::deploy::rerender_traefik(&state)?;
            state.domains.get_service_domain(d.id)?.unwrap()
        }
        Err(e) => {
            state.domains.update_service_domain_status(
                d.id,
                DomainStatus::Failed,
                None,
                Some(e.to_string()),
            )?;
            state.domains.get_service_domain(d.id)?.unwrap()
        }
    };
    Ok(Json(updated))
}

async fn delete_service_domain_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, domain_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<StatusCode, ApiError> {
    let svc = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let d = state
        .domains
        .get_service_domain(domain_id)?
        .ok_or_else(|| ApiError::NotFound("domain not found".into()))?;
    if d.service_id != service_id {
        return Err(ApiError::NotFound("domain not found".into()));
    }
    let was_verified = d.status == DomainStatus::Verified;
    state.domains.delete_service_domain(domain_id)?;
    if was_verified {
        crate::deploy::rerender_traefik(&state)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn lifecycle_command(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, action)): axum::extract::Path<(uuid::Uuid, String)>,
) -> Result<(StatusCode, Json<LifecycleResponse>), ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    match action.as_str() {
        "stop" => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.deployment_repos(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
                state.routes.clone(),
                state.ingress_options.clone(),
            );
            coordinator.stop_service(&service).await?;
            Ok((
                StatusCode::ACCEPTED,
                Json(LifecycleResponse { service_id, action }),
            ))
        }
        _ => Err(ApiError::BadRequest(format!(
            "unsupported lifecycle action: {action}"
        ))),
    }
}

async fn service_logs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    let logs = LogStore::new(&state.config.log_dir);
    match logs.read_recent(&service.name, 200) {
        Ok(lines) => Ok(Json(lines)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Json(Vec::new())),
        Err(error) => Err(ApiError::Log(error)),
    }
}

async fn service_metrics(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::metrics::MetricSnapshot>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    let Some(deployment_id) = state.deployments.promoted_deployment(service_id)? else {
        return Ok(Json(Vec::new()));
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    Ok(Json(vec![reader.read_by_id(
        &service.name,
        service.id,
        deployment_id,
    )?]))
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    if let Some(token) = token
        && let Some(principal) = resolve_auth(
            state.users.as_ref(),
            state.tokens.as_ref(),
            &token,
            &state.config.admin_token,
        )
    {
        let mut request = request;
        request.extensions_mut().insert(principal);
        return Ok(next.run(request).await);
    }
    Err(StatusCode::UNAUTHORIZED)
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<LoginRequest>,
) -> Result<Json<LoginResult>, ApiError> {
    if headers.get(header::AUTHORIZATION).is_some() {
        return Err(ApiError::BadRequest("already authenticated".to_string()));
    }
    let user = state
        .users
        .verify_login(&input.username, &input.password)
        .map_err(|_| ApiError::Unauthorized("invalid credentials".to_string()))?;
    let session = state.users.create_session(user.id, 24)?;
    Ok(Json(LoginResult {
        token: session.token,
        expires_at: session.expires_at,
    }))
}

async fn logout_handler(
    State(state): State<AppState>,
    request: Request,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(t) = token {
        let th = crate::auth::hash_token(t);
        let _ = state.users.delete_session(&th);
    }
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({"logged_out": true})),
    ))
}

async fn me_handler(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Me>, ApiError> {
    if principal.is_super_admin && !principal.is_authenticated() {
        return Ok(Json(Me {
            principal: PrincipalView::Bootstrap,
            is_super_admin: true,
            memberships: vec![],
        }));
    }
    let user_id = principal
        .user_id
        .ok_or(ApiError::Conflict("no user".to_string()))?;
    let user = state
        .users
        .get_user(user_id)?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    Ok(Json(Me {
        principal: PrincipalView::User { user },
        is_super_admin: principal.is_super_admin,
        memberships,
    }))
}

async fn list_users(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<crate::domain::User>>, ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    Ok(Json(state.users.list_users()?))
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default)]
    is_super_admin: bool,
}

async fn create_user_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    let hash = crate::auth::hash_password(&input.password)?;
    state
        .users
        .create_user(&input.username, &hash, input.is_super_admin)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"created": true})),
    ))
}

async fn delete_user_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(user_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    state.users.delete_user(user_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

#[derive(Debug, Deserialize)]
struct CreateApiTokenRequest {
    name: String,
}

async fn list_api_tokens_handler(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<ApiToken>>, ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    Ok(Json(state.tokens.list_api_tokens(user_id)?))
}

async fn create_api_token_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateApiTokenRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    let api_token = state.tokens.create_api_token(user_id, &input.name)?;
    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({"id": api_token.id.to_string(), "name": api_token.name, "token": api_token.token}),
        ),
    ))
}

async fn revoke_api_token_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(token_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    let tokens = state.tokens.list_api_tokens(user_id)?;
    let belongs = tokens.iter().any(|t| t.id == token_id);
    if !belongs {
        return Err(ApiError::NotFound("token not found".to_string()));
    }
    state.tokens.revoke_api_token(token_id)?;
    Ok(Json(serde_json::json!({"revoked": true})))
}

async fn list_jobs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let project_id = params
        .get("project_id")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or_else(|| ApiError::BadRequest("project_id query parameter is required".into()))?;
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    Ok(Json(state.jobs.list_jobs(project_id)?))
}

async fn create_job(
    State(state): State<AppState>,
    principal: Principal,
    Json(job): Json<Job>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
    let stored = state.jobs.put_job(job)?;
    Ok((StatusCode::CREATED, Json(stored)))
}

async fn get_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Job>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_role(&state, &principal, job.project_id, Role::Viewer)?;
    Ok(Json(job))
}

async fn delete_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
    state.jobs.delete_job(job_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

async fn run_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<(StatusCode, Json<JobRun>), ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
    if state.jobs.active_run(job_id)?.is_some() {
        return Err(ApiError::Conflict(
            "job already has an active run".to_string(),
        ));
    }
    let run = state.jobs.create_job_run(job_id)?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn list_job_runs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<JobRun>>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_role(&state, &principal, job.project_id, Role::Viewer)?;
    Ok(Json(state.jobs.list_job_runs(job_id)?))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Debug, Deserialize)]
struct CredentialInput {
    name: String,
    kind: CredentialKind,
    secret_ref: String,
}

#[derive(Debug, Serialize)]
struct LifecycleResponse {
    service_id: uuid::Uuid,
    action: String,
}

#[derive(Debug)]
pub enum ApiError {
    State(crate::state::StateError),
    Repo(RepoError),
    Auth(crate::auth::AuthError),
    InvalidSecretRef(crate::secrets::SecretRefError),
    BadRequest(String),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    Deploy(DeployError),
    Log(std::io::Error),
    Metrics(MetricsError),
    NodeMetrics(NodeMetricsError),
}

impl From<crate::auth::AuthError> for ApiError {
    fn from(value: crate::auth::AuthError) -> Self {
        match value {
            crate::auth::AuthError::InvalidCredentials => {
                ApiError::Unauthorized("invalid credentials".to_string())
            }
            crate::auth::AuthError::Forbidden => ApiError::Forbidden("forbidden".to_string()),
            crate::auth::AuthError::InvalidToken => {
                ApiError::Unauthorized("invalid token".to_string())
            }
            crate::auth::AuthError::State(e) => ApiError::State(e),
        }
    }
}

impl From<crate::state::StateError> for ApiError {
    fn from(value: crate::state::StateError) -> Self {
        Self::State(value)
    }
}

impl From<RepoError> for ApiError {
    fn from(value: RepoError) -> Self {
        Self::Repo(value)
    }
}

impl From<DeployError> for ApiError {
    fn from(value: DeployError) -> Self {
        Self::Deploy(value)
    }
}

impl From<MetricsError> for ApiError {
    fn from(value: MetricsError) -> Self {
        Self::Metrics(value)
    }
}

impl From<NodeMetricsError> for ApiError {
    fn from(value: NodeMetricsError) -> Self {
        Self::NodeMetrics(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::State(error) => match &error {
                crate::state::StateError::ProjectNotEmpty => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                crate::state::StateError::UnknownProject => {
                    (StatusCode::NOT_FOUND, error.to_string())
                }
                crate::state::StateError::RegistryNotFound => {
                    (StatusCode::NOT_FOUND, error.to_string())
                }
                crate::state::StateError::RegistryInUse => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                crate::state::StateError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::state::StateError::LastSuperAdmin => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Repo(error) => match &error {
                RepoError::ProjectNotEmpty => (StatusCode::CONFLICT, error.to_string()),
                RepoError::UnknownProject => (StatusCode::NOT_FOUND, error.to_string()),
                RepoError::RegistryNotFound => (StatusCode::NOT_FOUND, error.to_string()),
                RepoError::RegistryInUse => (StatusCode::CONFLICT, error.to_string()),
                RepoError::InvalidCredentials => (StatusCode::UNAUTHORIZED, error.to_string()),
                RepoError::LastSuperAdmin => (StatusCode::CONFLICT, error.to_string()),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Auth(error) => match &error {
                crate::auth::AuthError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::Forbidden => (StatusCode::FORBIDDEN, error.to_string()),
                crate::auth::AuthError::InvalidToken => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::State(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::InvalidSecretRef(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Deploy(error) => match &error {
                DeployError::RegistryNotFound => (StatusCode::NOT_FOUND, error.to_string()),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Log(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
            Self::Metrics(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
            Self::NodeMetrics(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
        };
        (status, message).into_response()
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}
