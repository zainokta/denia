use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{Registry, RegistryAuthKind, Role};
use crate::secrets::{SecretPayload, SecretRef, SopsSecretStore};

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
#[serde(tag = "auth_kind", rename_all = "snake_case")]
enum RegistryInputAuth {
    Anonymous,
    Basic { username: String, password: String },
    Token { token: String },
    EcrToken { token: String },
    GarToken { token: String },
}

#[derive(Debug, Deserialize)]
struct RegistryInput {
    name: String,
    endpoint: String,
    #[serde(flatten)]
    auth: RegistryInputAuth,
}

impl RegistryInputAuth {
    fn kind(&self) -> RegistryAuthKind {
        match self {
            Self::Anonymous => RegistryAuthKind::Anonymous,
            Self::Basic { .. } => RegistryAuthKind::Basic,
            Self::Token { .. } => RegistryAuthKind::Token,
            Self::EcrToken { .. } => RegistryAuthKind::EcrToken,
            Self::GarToken { .. } => RegistryAuthKind::GarToken,
        }
    }

    fn payload(&self) -> Option<SecretPayload> {
        match self {
            Self::Anonymous => None,
            Self::Basic { username, password } => {
                Some(SecretPayload::new(format!("{username}:{password}")))
            }
            Self::Token { token } | Self::EcrToken { token } | Self::GarToken { token } => {
                Some(SecretPayload::new(token.clone()))
            }
        }
    }
}

async fn list_registries(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<Registry>>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    Ok(Json(state.registries.registries_for_project(project_id)?))
}

async fn encrypt_payload(
    state: &AppState,
    project_id: uuid::Uuid,
    secret_ref: &SecretRef,
    payload: &SecretPayload,
) -> Result<(), ApiError> {
    let recipient = state.config.age_recipient.as_deref().ok_or_else(|| {
        ApiError::BadRequest("control plane has no DENIA_AGE_RECIPIENT configured".into())
    })?;
    let store = SopsSecretStore::new(state.config.data_dir.clone());
    store
        .encrypt(
            state.command_runner.as_ref(),
            state.config.sops_binary.as_path(),
            recipient,
            project_id,
            secret_ref,
            payload,
        )
        .await
        .map_err(|e| ApiError::BadRequest(format!("secret encryption failed: {e}")))?;
    Ok(())
}

async fn create_registry(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
    Json(input): Json<RegistryInput>,
) -> Result<(StatusCode, Json<Registry>), ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let auth_kind = input.auth.kind();
    let credential_ref = match input.auth.payload() {
        Some(payload) => {
            let secret_ref = SecretRef::generate("registry");
            encrypt_payload(&state, project_id, &secret_ref, &payload).await?;
            Some(secret_ref)
        }
        None => None,
    };
    let registry = Registry::new(
        project_id,
        input.name,
        input.endpoint,
        auth_kind,
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
) -> Result<Json<Registry>, ApiError> {
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
) -> Result<Json<Registry>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let existing = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if existing.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    let auth_kind = input.auth.kind();
    // Reuse the existing ref so the encrypted file is overwritten in-place
    // and any ServiceConfig rows referencing it stay valid.
    let credential_ref = match (input.auth.payload(), existing.credential_ref.clone()) {
        (Some(payload), Some(prev_ref)) => {
            encrypt_payload(&state, project_id, &prev_ref, &payload).await?;
            Some(prev_ref)
        }
        (Some(payload), None) => {
            let secret_ref = SecretRef::generate("registry");
            encrypt_payload(&state, project_id, &secret_ref, &payload).await?;
            Some(secret_ref)
        }
        (None, _) => None,
    };
    let mut updated = Registry::new(
        project_id,
        input.name,
        input.endpoint,
        auth_kind,
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
