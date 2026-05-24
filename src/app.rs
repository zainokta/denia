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

use crate::{
    config::AppConfig,
    domain::{Credential, CredentialKind, DeploymentRequest, ServiceConfig},
    secrets::SecretRef,
    state::SqliteStore,
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub store: SqliteStore,
}

impl AppState {
    pub fn new(config: AppConfig, store: SqliteStore) -> Self {
        Self { config, store }
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
        .route("/services/{service_id}/{action}", post(lifecycle_command))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", protected)
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
    Ok(Json(state.store.create_deployment(request)?))
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
    axum::extract::Path((service_id, action)): axum::extract::Path<(uuid::Uuid, String)>,
) -> (StatusCode, Json<LifecycleResponse>) {
    (
        StatusCode::ACCEPTED,
        Json(LifecycleResponse { service_id, action }),
    )
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
}

impl From<crate::state::StateError> for ApiError {
    fn from(value: crate::state::StateError) -> Self {
        Self::State(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::State(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::InvalidSecretRef(error) => (StatusCode::BAD_REQUEST, error.to_string()),
        };
        (status, message).into_response()
    }
}
