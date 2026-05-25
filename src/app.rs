use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;

use crate::{
    artifacts::acquirer::ArtifactAcquirer,
    bridge::{BridgeAllocator, BridgeManager, LoopbackBridgeSupervisor},
    command::{CommandRunner, TokioCommandRunner},
    config::AppConfig,
    deploy::{DeployError, DeploymentCoordinator},
    domain::{Credential, CredentialKind, DeploymentRequest, ServiceConfig},
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
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", protected)
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
    InvalidSecretRef(crate::secrets::SecretRefError),
    BadRequest(String),
    NotFound(String),
    Deploy(DeployError),
    Log(std::io::Error),
    Metrics(MetricsError),
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
            Self::State(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::InvalidSecretRef(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Deploy(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Log(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Metrics(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (status, message).into_response()
    }
}
