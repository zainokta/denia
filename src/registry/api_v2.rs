//! Hosted OCI registry `/v2` routes backed by the Denia-managed local Zot.
//!
//! Denia remains the public authentication and project-RBAC boundary. After a
//! request is authenticated and authorized for `<project>/<service>`, its body
//! is streamed to Zot on `127.0.0.1:5000`. Zot therefore owns registry blob,
//! manifest, upload, dedupe, and GC mechanics without becoming publicly
//! reachable.

use std::sync::OnceLock;

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::Role;
use crate::registry::domain::HostedRepository;

const ZOT_BASE_URL: &str = "http://127.0.0.1:5000";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", any(proxy_v2))
        .route("/{*path}", any(proxy_v2))
}

/// Auth middleware for `/v2`. Accepts the same bearer tokens as `/v1` AND HTTP
/// Basic auth where the PASSWORD is a Denia API token. Zot is intentionally
/// unaware of public credentials; the Authorization header is stripped before
/// proxying to the loopback service.
pub(crate) async fn registry_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let api_version = HeaderName::from_static("docker-distribution-api-version");
    if let Some(token) = extract_registry_token(request.headers())
        && let Some(principal) = crate::auth::resolve_auth(
            &state.users,
            &state.tokens,
            &token,
            &state.config.admin_token_hash,
            &state.config.admin_token_hmac_key,
        )
    {
        let mut request = request;
        request.extensions_mut().insert(principal);
        let mut resp = next.run(request).await;
        resp.headers_mut()
            .insert(api_version, HeaderValue::from_static("registry/2.0"));
        return resp;
    }

    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Denia Registry\""),
    );
    headers.insert(api_version, HeaderValue::from_static("registry/2.0"));
    resp
}

fn extract_registry_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if let Some(bearer) = value.strip_prefix("Bearer ") {
        return Some(bearer.to_string());
    }
    if let Some(basic) = value.strip_prefix("Basic ") {
        let decoded = STANDARD.decode(basic.trim()).ok()?;
        let creds = String::from_utf8(decoded).ok()?;
        let (_user, token) = creds.split_once(':')?;
        return Some(token.to_string());
    }
    None
}

async fn proxy_v2(
    State(state): State<AppState>,
    OriginalUri(original_uri): OriginalUri,
    principal: Principal,
    request: Request,
) -> Result<Response, ApiError> {
    let method = request.method().clone();
    let path = original_uri.path();

    if path != "/v2" && path != "/v2/" {
        let Some((project, service)) = repository_segments(path)? else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };
        let Some(role) = required_role(&method) else {
            return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
        };
        resolve_repo(&state, &principal, project, service, role)?;
    }

    let upstream_url = format!("{ZOT_BASE_URL}{original_uri}");
    let (parts, body) = request.into_parts();
    let mut upstream_request = zot_client()
        .request(method, upstream_url)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));

    for (name, value) in &parts.headers {
        if !is_hop_by_hop(name) && name != header::AUTHORIZATION && name != header::HOST {
            upstream_request = upstream_request.header(name, value);
        }
    }

    let upstream = match upstream_request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "local Zot registry request failed");
            return Ok((StatusCode::BAD_GATEWAY, "registry backend unavailable").into_response());
        }
    };

    let status = upstream.status();
    let headers = upstream.headers().clone();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;

    for (name, value) in &headers {
        if is_hop_by_hop(name) {
            continue;
        }
        if name == header::LOCATION {
            if let Some(value) = rewrite_location(value) {
                response.headers_mut().insert(name.clone(), value);
            }
            continue;
        }
        response.headers_mut().insert(name.clone(), value.clone());
    }

    Ok(response)
}

fn zot_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

fn validate_segment(seg: &str) -> Result<(), ApiError> {
    if seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.contains('/')
        || seg.contains('\\')
        || seg.chars().any(|c| c.is_whitespace())
    {
        return Err(ApiError::BadRequest("invalid repository name".to_string()));
    }
    Ok(())
}

/// Parse the Denia hosted-repository namespace from an original `/v2/...`
/// request. Denia intentionally supports exactly `<project>/<service>` as the
/// repository name, preserving ADR-031's authorization mapping.
fn repository_segments(path: &str) -> Result<Option<(&str, &str)>, ApiError> {
    let Some(rest) = path.strip_prefix("/v2/") else {
        return Ok(None);
    };
    let mut segments = rest.split('/');
    let Some(project) = segments.next() else {
        return Ok(None);
    };
    let Some(service) = segments.next() else {
        return Ok(None);
    };
    validate_segment(project)?;
    validate_segment(service)?;
    Ok(Some((project, service)))
}

fn required_role(method: &Method) -> Option<Role> {
    match *method {
        Method::GET | Method::HEAD => Some(Role::Viewer),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => Some(Role::Operator),
        _ => None,
    }
}

/// Resolve `<project>/<service>` to a hosted repository, enforcing `min` role.
/// The metadata row is retained as Denia's authorization/audit mapping even
/// though Zot owns the registry content itself.
fn resolve_repo(
    state: &AppState,
    principal: &Principal,
    project: &str,
    service: &str,
    min: Role,
) -> Result<HostedRepository, ApiError> {
    let proj = state
        .projects
        .list_projects()?
        .into_iter()
        .find(|p| p.name == project)
        .ok_or_else(|| ApiError::NotFound("unknown project".to_string()))?;
    let svc = state
        .services
        .list_services()?
        .into_iter()
        .find(|s| s.project_id == proj.id && s.name == service)
        .ok_or_else(|| ApiError::NotFound("unknown service".to_string()))?;
    ensure_role(state, principal, proj.id, min)?;
    state
        .registry
        .ensure_repository(proj.id, svc.id, &format!("{project}/{service}"))
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "proxy-connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Zot may generate an absolute upload `Location` using its loopback address.
/// Clients must continue through Denia, so strip the internal origin while
/// preserving the Distribution path/query.
fn rewrite_location(value: &HeaderValue) -> Option<HeaderValue> {
    let text = value.to_str().ok()?;
    let rewritten = text.strip_prefix(ZOT_BASE_URL).unwrap_or(text);
    HeaderValue::from_str(rewritten).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bearer_and_basic_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        assert_eq!(extract_registry_token(&headers).as_deref(), Some("secret-token"));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjp0b2tlbg=="),
        );
        assert_eq!(extract_registry_token(&headers).as_deref(), Some("token"));
    }

    #[test]
    fn parses_project_and_service_from_distribution_path() {
        assert_eq!(
            repository_segments("/v2/acme/api/manifests/latest").unwrap(),
            Some(("acme", "api"))
        );
        assert_eq!(repository_segments("/v2/").unwrap(), None);
        assert!(repository_segments("/v2/acme").unwrap().is_none());
    }

    #[test]
    fn maps_pull_and_push_methods_to_existing_rbac() {
        assert_eq!(required_role(&Method::GET), Some(Role::Viewer));
        assert_eq!(required_role(&Method::HEAD), Some(Role::Viewer));
        assert_eq!(required_role(&Method::PUT), Some(Role::Operator));
        assert_eq!(required_role(&Method::PATCH), Some(Role::Operator));
        assert_eq!(required_role(&Method::DELETE), Some(Role::Operator));
        assert_eq!(required_role(&Method::OPTIONS), None);
    }

    #[test]
    fn strips_loopback_origin_from_zot_upload_location() {
        let value = HeaderValue::from_static(
            "http://127.0.0.1:5000/v2/acme/api/blobs/uploads/abc?_state=xyz",
        );
        assert_eq!(
            rewrite_location(&value).unwrap(),
            HeaderValue::from_static("/v2/acme/api/blobs/uploads/abc?_state=xyz")
        );
    }

    #[test]
    fn rejects_invalid_repository_segments() {
        assert!(repository_segments("/v2/../api/manifests/latest").is_err());
        assert!(repository_segments("/v2/acme /api/manifests/latest").is_err());
    }
}
