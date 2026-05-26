use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::Role;
use crate::secrets::SecretRef;

pub fn router() -> Router<AppState> {
    Router::new()
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
