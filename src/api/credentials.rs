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
    // Only the git-key path remains. Registry credentials are encrypted by the
    // control plane on the registry CRUD path (ADR-021); the legacy
    // `/credentials/registry` route that accepted an operator-managed
    // `secret_ref` has been removed.
    Router::new()
        .route("/credentials", get(list_credentials))
        .route("/credentials/git", post(put_credential))
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
    // ADR-021: registry credentials must not be wired via an operator-managed
    // `secret_ref`. They are encrypted by the control plane on the registry CRUD
    // path (`POST/PATCH /v1/projects/{id}/registries`). This endpoint only
    // accepts SSH deploy keys.
    if !matches!(input.kind, CredentialKind::SshDeployKey) {
        return Err(ApiError::BadRequest(
            "registry credentials must be managed via the registry CRUD endpoints (ADR-021)"
                .to_string(),
        ));
    }
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn put_git_credential(app: &axum::Router, body: serde_json::Value) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/credentials/git")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn ssh_deploy_key_accepted() {
        let app = build_router(test_state());
        let body = serde_json::json!({
            "name": "deploy-key",
            "kind": "SshDeployKey",
            "secret_ref": "git-deploy-key",
        });
        assert_eq!(put_git_credential(&app, body).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn registry_kind_rejected_with_400() {
        let app = build_router(test_state());
        for kind in ["RegistryBasic", "RegistryToken"] {
            let body = serde_json::json!({
                "name": "reg",
                "kind": kind,
                "secret_ref": "some-ref",
            });
            assert_eq!(
                put_git_credential(&app, body).await,
                StatusCode::BAD_REQUEST,
                "kind {kind} must be rejected (ADR-021)"
            );
        }
    }

    /// The legacy `/credentials/registry` route is gone (ADR-021). A POST there
    /// now falls through to the SPA fallback and must NOT create a credential
    /// row via `put_credential`.
    #[tokio::test]
    async fn registry_route_no_longer_creates_a_credential() {
        let state = test_state();
        let body = serde_json::json!({
            "name": "reg",
            "kind": "SshDeployKey",
            "secret_ref": "some-ref",
        });
        let _ = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/credentials/registry")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The route no longer maps to `put_credential`, so nothing is persisted.
        assert!(
            state.credentials.list_credentials().unwrap().is_empty(),
            "POST /credentials/registry must not persist a credential"
        );
    }
}
