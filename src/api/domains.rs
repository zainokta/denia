use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, post},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{DomainStatus, Role, ServiceDomain};
use crate::repo::RepoError;

/// A service may not claim the control-plane hostname. Both sides are
/// lowercased at their sources (`validate_hostname` and config validation).
pub(crate) fn is_reserved_control_hostname(hostname: &str, control_domain: Option<&str>) -> bool {
    control_domain == Some(hostname)
}

pub fn router() -> Router<AppState> {
    Router::new()
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
}

pub async fn challenge_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let hostname = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(challenge_host_name)
        .ok_or_else(|| ApiError::NotFound("not found".into()))?;
    let found = state.domains.get_service_domain_by_token(&token)?;
    if found.is_some_and(|domain| domain.hostname == hostname) {
        Ok(([(header::CONTENT_TYPE, "text/plain")], token).into_response())
    } else {
        Err(ApiError::NotFound("not found".into()))
    }
}

fn challenge_host_name(host: &str) -> Option<String> {
    let host = host.trim();
    let without_port = if host.matches(':').count() == 1 {
        let (name, port) = host.rsplit_once(':')?;
        if port.bytes().all(|b| b.is_ascii_digit()) {
            name
        } else {
            host
        }
    } else {
        host
    };
    crate::verification::validate_hostname(without_port).ok()
}

/// ACME HTTP-01 challenge responder.
///
/// Serves the key authorization registered by the in-process ACME driver for
/// `token` (via the shared [`ChallengeStore`]). Pingora's `:80` listener proxies
/// `/.well-known/acme-challenge/{token}` here. Unknown tokens return 404.
///
/// [`ChallengeStore`]: crate::ingress::pingora::acme::ChallengeStore
pub async fn acme_challenge_handler(
    State(state): State<AppState>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<axum::response::Response, ApiError> {
    match state.acme_challenges.get(&token) {
        Some(key_authorization) => {
            Ok(([(header::CONTENT_TYPE, "text/plain")], key_authorization).into_response())
        }
        None => Err(ApiError::NotFound("not found".into())),
    }
}

#[derive(Deserialize)]
struct CreateDomainBody {
    hostname: String,
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

    if is_reserved_control_hostname(&hostname, state.config.control_domain.as_deref()) {
        return Err(ApiError::Conflict(
            "hostname is reserved for the control plane".into(),
        ));
    }

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
        RepoError::Sqlite(ref err) if crate::api::error::is_constraint_violation(err) => {
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
    if is_reserved_control_hostname(&d.hostname, state.config.control_domain.as_deref()) {
        return Err(ApiError::Conflict(
            "hostname is reserved for the control plane".into(),
        ));
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

    // Release the in-flight guard. Use the same graceful poisoned-lock handling
    // as the acquire path above so a poisoned mutex can never panic the worker.
    if let Ok(mut guard) = state.verifying_domains.lock() {
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
            crate::deploy::apply_routes(&state)?;
            // On-demand TLS issuance (review HIGH): a freshly verified domain on
            // a tls_enabled service must get a cert immediately so `:443` can
            // serve it within seconds, not after the next 12h renewal scan. The
            // hostname is already validated/lowercased by `validate_hostname`;
            // the ACME task re-validates before issuing.
            if svc.tls_enabled {
                crate::ingress::pingora::request_issue(&state.cert_issue_tx, d.hostname.clone());
            }
            // The row can be deleted concurrently between the status write and
            // this re-read; surface a typed 404 rather than panicking.
            state
                .domains
                .get_service_domain(d.id)?
                .ok_or_else(|| ApiError::NotFound("domain not found".into()))?
        }
        Err(e) => {
            state.domains.update_service_domain_status(
                d.id,
                DomainStatus::Failed,
                None,
                Some(e.to_string()),
            )?;
            state
                .domains
                .get_service_domain(d.id)?
                .ok_or_else(|| ApiError::NotFound("domain not found".into()))?
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
        crate::deploy::apply_routes(&state)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn control_hostname_is_reserved() {
        use super::is_reserved_control_hostname;
        assert!(is_reserved_control_hostname(
            "denia.example.com",
            Some("denia.example.com")
        ));
        assert!(!is_reserved_control_hostname(
            "svc.example.com",
            Some("denia.example.com")
        ));
        assert!(!is_reserved_control_hostname("denia.example.com", None));
    }

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    /// Proves the root-level unauthenticated challenge mount + token lookup:
    /// an unknown token returns 404 without any bearer credential.
    #[tokio::test]
    async fn challenge_unknown_token_returns_404() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/denia-challenge/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn acme_challenge_unknown_token_returns_404() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/acme-challenge/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn acme_challenge_returns_registered_key_authorization() {
        let state = test_state();
        // Register a challenge response in the shared store; the router clones
        // the same `Arc`, so the handler sees it.
        state
            .acme_challenges
            .register("tok-abc", "tok-abc.keyauth-value");
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/.well-known/acme-challenge/tok-abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"tok-abc.keyauth-value");
    }
}
