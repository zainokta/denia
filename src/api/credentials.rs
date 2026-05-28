use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};
use crate::domain::{Credential, CredentialKind};
use crate::secrets::SecretRef;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/credentials", get(list_credentials))
        .route("/credentials/git", post(put_credential))
        .route("/credentials/registry", post(put_credential))
}

#[derive(Debug, Deserialize)]
struct CredentialInput {
    name: String,
    kind: CredentialKind,
    secret_ref: String,
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

async fn list_credentials(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<Credential>>, ApiError> {
    ensure_super_admin(&principal)?;
    Ok(Json(state.credentials.list_credentials()?))
}
