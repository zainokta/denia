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
use std::sync::Arc;

use crate::{
    artifacts::acquirer::ArtifactAcquirer,
    auth::{Principal, resolve_auth},
    bridge::{BridgeAllocator, BridgeManager, LoopbackBridgeSupervisor},
    command::{CommandRunner, TokioCommandRunner},
    config::AppConfig,
    deploy::{DeployError, DeploymentCoordinator},
    domain::{
        ApiToken, Credential, CredentialKind, DeploymentRequest, Job, JobRun, LoginResult, Me,
        PrincipalView, Project, ServiceConfig,
    },
    health::{FakeHealthChecker, HealthChecker},
    logs::LogStore,
    metrics::{CgroupMetricsReader, MetricsError},
    runtime::{LinuxRuntime, Runtime},
    secrets::SecretRef,
    state::SqliteStore,
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub store: SqliteStore,
    runtime: Arc<dyn Runtime>,
    health: Arc<dyn HealthChecker>,
    command_runner: Arc<dyn CommandRunner>,
    bridge_allocator: Arc<std::sync::Mutex<BridgeAllocator>>,
    bridge_manager: Arc<dyn BridgeManager>,
}

impl AppState {
    pub fn new(config: AppConfig, store: SqliteStore) -> Self {
        let bridge_start_port = config.bridge_start_port;
        let runtime = Arc::new(
            LinuxRuntime::new_with_paths(
                config.runtime_dir.clone(),
                config.artifact_dir.clone(),
                config.cgroup_root.clone(),
            )
            .with_userns(config.userns_base, config.userns_size)
            .with_setpriv(config.setpriv_binary.clone())
            .with_log_dir(config.log_dir.clone()),
        );
        Self::new_with_deploy_dependencies(
            config,
            store,
            runtime,
            FakeHealthChecker::healthy(),
            TokioCommandRunner,
            BridgeAllocator::new(bridge_start_port),
            LoopbackBridgeSupervisor::default(),
        )
    }

    pub fn new_with_deploy_dependencies<R, H, C, B, M>(
        config: AppConfig,
        store: SqliteStore,
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
        Self {
            config,
            store,
            runtime: Arc::new(runtime),
            health: Arc::new(health),
            command_runner: Arc::new(command_runner),
            bridge_allocator: Arc::new(std::sync::Mutex::new(bridge_allocator.into())),
            bridge_manager: Arc::new(bridge_manager),
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let auth_public = Router::new().route("/auth/login", post(login_handler));

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
        .route("/services/{service_id}/{action}", post(lifecycle_command))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).delete(delete_project),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", auth_public.merge(auth_routes).merge(protected))
        .fallback(crate::web::static_handler)
        .with_state(state)
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn list_services(
    State(state): State<AppState>,
) -> Result<Json<Vec<ServiceConfig>>, ApiError> {
    Ok(Json(state.store.list_services()?))
}

async fn put_service(
    State(state): State<AppState>,
    Json(service): Json<ServiceConfig>,
) -> Result<Json<ServiceConfig>, ApiError> {
    Ok(Json(state.store.put_service(service)?))
}

async fn create_deployment(
    State(state): State<AppState>,
    Json(request): Json<DeploymentRequest>,
) -> Result<Json<crate::domain::Deployment>, ApiError> {
    let Some(service) = state.store.get_service(request.service_id())? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    match request {
        DeploymentRequest::ExternalImage { .. } => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.store.clone(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
            );
            let acquirer = ArtifactAcquirer::new(state.config.clone());
            Ok(Json(
                coordinator
                    .deploy_external_image_source(
                        &service,
                        &acquirer,
                        state.command_runner.as_ref(),
                    )
                    .await?,
            ))
        }
        DeploymentRequest::Git { .. } => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.store.clone(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
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

async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(state.store.list_projects()?))
}

async fn get_project(
    State(state): State<AppState>,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Project>, ApiError> {
    let project = state
        .store
        .get_project(project_id)?
        .ok_or_else(|| ApiError::NotFound("project not found".to_string()))?;
    Ok(Json(project))
}

async fn create_project(
    State(state): State<AppState>,
    Json(project): Json<Project>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(state.store.put_project(project)?))
}

async fn delete_project(
    State(state): State<AppState>,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.store.delete_project(project_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

async fn put_credential(
    State(state): State<AppState>,
    Json(input): Json<CredentialInput>,
) -> Result<Json<Credential>, ApiError> {
    let secret_ref = SecretRef::parse(input.secret_ref).map_err(ApiError::InvalidSecretRef)?;
    Ok(Json(
        state
            .store
            .put_credential(input.name, input.kind, secret_ref)?,
    ))
}

async fn list_service_deployments(
    State(state): State<AppState>,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::domain::Deployment>>, ApiError> {
    Ok(Json(state.store.list_deployments(service_id)?))
}

async fn lifecycle_command(
    State(state): State<AppState>,
    axum::extract::Path((service_id, action)): axum::extract::Path<(uuid::Uuid, String)>,
) -> Result<(StatusCode, Json<LifecycleResponse>), ApiError> {
    let Some(service) = state.store.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    match action.as_str() {
        "stop" => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.store.clone(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
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
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    let Some(service) = state.store.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    let logs = LogStore::new(&state.config.log_dir);
    match logs.read_recent(&service.name, 200) {
        Ok(lines) => Ok(Json(lines)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Json(Vec::new())),
        Err(error) => Err(ApiError::Log(error)),
    }
}

async fn service_metrics(
    State(state): State<AppState>,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::metrics::MetricSnapshot>>, ApiError> {
    let Some(service) = state.store.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    let Some(deployment_id) = state.store.promoted_deployment(service_id)? else {
        return Ok(Json(Vec::new()));
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    Ok(Json(vec![
        reader.read_service(&service.name, deployment_id)?,
    ]))
}

async fn require_admin_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = format!("Bearer {}", state.config.admin_token);
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);

    if !authorized {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
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
        && let Some(principal) = resolve_auth(&state.store, &token, &state.config.admin_token)
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
        .store
        .verify_login(&input.username, &input.password)
        .map_err(|_| ApiError::Conflict("invalid credentials".to_string()))?;
    let session = state.store.create_session(user.id, 24)?;
    Ok(Json(LoginResult {
        token: session.token_hash,
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
        let _ = state.store.delete_session(&th);
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
        .store
        .get_user(user_id)?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;
    let memberships = state.store.list_memberships_for_user(user_id)?;
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
    Ok(Json(state.store.list_users()?))
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
        .store
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
    state.store.delete_user(user_id)?;
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
    Ok(Json(state.store.list_api_tokens(user_id)?))
}

async fn create_api_token_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateApiTokenRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    let api_token = state.store.create_api_token(user_id, &input.name)?;
    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({"id": api_token.id.to_string(), "name": api_token.name, "token": api_token.token_hash}),
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
    let tokens = state.store.list_api_tokens(user_id)?;
    let belongs = tokens.iter().any(|t| t.id == token_id);
    if !belongs {
        return Err(ApiError::NotFound("token not found".to_string()));
    }
    state.store.revoke_api_token(token_id)?;
    Ok(Json(serde_json::json!({"revoked": true})))
}

async fn list_jobs(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let project_id = params
        .get("project_id")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .unwrap_or(uuid::Uuid::nil());
    Ok(Json(state.store.list_jobs(project_id)?))
}

async fn create_job(
    State(state): State<AppState>,
    Json(job): Json<Job>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let stored = state.store.put_job(job)?;
    Ok((StatusCode::CREATED, Json(stored)))
}

async fn get_job(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Job>, ApiError> {
    let job = state
        .store
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    Ok(Json(job))
}

async fn delete_job(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.store.delete_job(job_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

async fn run_job(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<(StatusCode, Json<JobRun>), ApiError> {
    let _job = state
        .store
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    let run = state.store.create_job_run(job_id)?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn list_job_runs(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<JobRun>>, ApiError> {
    Ok(Json(state.store.list_job_runs(job_id)?))
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
                crate::state::StateError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::state::StateError::LastSuperAdmin => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            },
            Self::Auth(error) => match &error {
                crate::auth::AuthError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::Forbidden => (StatusCode::FORBIDDEN, error.to_string()),
                crate::auth::AuthError::InvalidToken => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::State(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                }
            },
            Self::InvalidSecretRef(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Deploy(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Log(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Metrics(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (status, message).into_response()
    }
}
