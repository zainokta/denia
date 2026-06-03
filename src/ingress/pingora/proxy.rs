//! `DeniaProxy` — the Pingora `ProxyHttp` request-routing brain.
//!
//! This is the L7 request path: it resolves a request `Host` to a service via
//! the [`RouteTable`], picks a healthy replica (firing scale-from-zero
//! activation when the pool is cold), and proxies to the workload's
//! Denia-owned Unix socket. On `:80` it also intercepts ACME / denia challenge
//! paths (proxied to the control backend) and issues HTTP→HTTPS redirects for
//! TLS-enabled services.
//!
//! ## Testability
//!
//! All decision logic lives in free functions (`classify_port80`,
//! `select_upstream`) that take plain inputs and return plain enums, so they are
//! unit-tested without a live Pingora `Session` or socket. The `ProxyHttp`
//! trait methods are thin adapters that pull request fields off the `Session`,
//! call these helpers, and translate the result into Pingora actions.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use pingora::http::ResponseHeader;
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType};
use pingora_proxy::{ProxyHttp, Session};

use crate::observability::access_log::{AccessEntry, sanitize_path};

use super::state::{ActivationError, IngressState};

/// ACME HTTP-01 challenge path prefix (served by the axum control plane).
const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";
/// Denia domain-verification challenge path prefix (ADR-013, axum control plane).
const DENIA_CHALLENGE_PREFIX: &str = "/.well-known/denia-challenge/";

/// Decision produced by the `:80` `request_filter` classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Port80Decision {
    /// Path is an ACME / denia challenge: proxy to the control backend,
    /// bypassing host routing entirely (wins before any 404).
    ToControlBackend,
    /// Host resolved to a TLS-enabled service: send a 308 redirect to this
    /// `https://` location and finish the request.
    Redirect(String),
    /// Plain-HTTP service (or unknown host): fall through to `upstream_peer`,
    /// which performs host resolution and 404/replica selection.
    Passthrough,
}

/// Classify a `:80` request *before* host resolution.
///
/// Challenge paths win unconditionally (even for an unknown host) so external
/// ACME / domain verification always reaches axum. Otherwise, a TLS-enabled
/// matched service triggers a 308 redirect to `https://<host><path>`; anything
/// else passes through.
///
/// `tls_for_host` is the `tls` flag of the route the `Host` resolved to, or
/// `None` when the host is unrouted. `host`/`path` are taken verbatim from the
/// request line (already-decoded by Pingora). The redirect target is built from
/// the *validated* host (the caller resolved it through the `RouteTable`, whose
/// keys are validated/lowercased at ingest — audit A1/A2).
pub fn classify_port80(path: &str, host: &str, tls_for_host: Option<bool>) -> Port80Decision {
    if path.starts_with(ACME_CHALLENGE_PREFIX) || path.starts_with(DENIA_CHALLENGE_PREFIX) {
        return Port80Decision::ToControlBackend;
    }
    match tls_for_host {
        Some(true) => Port80Decision::Redirect(format!("https://{host}{path}")),
        _ => Port80Decision::Passthrough,
    }
}

/// Outcome of replica selection in `upstream_peer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamChoice {
    /// Proxy to the workload Unix socket at this path.
    Uds(std::path::PathBuf),
    /// Proxy to the control-plane backend (challenge interception).
    ControlBackend,
    /// Host did not resolve to any service → 404.
    NotFound,
    /// Service is scaled to zero and activation did not yield a replica in
    /// time (or no activator is configured) → 503.
    Unavailable,
}

/// Map an activation/resolution result to an [`UpstreamChoice`].
///
/// Pure decision step extracted so the zero/timeout/404 mapping is unit-tested
/// without a live `Session`. `resolved` is the output of
/// `IngressState::resolve_or_activate`:
/// - `Ok(Some(path))` → proxy to that UDS,
/// - `Ok(None)` (no activator / cold) → 503,
/// - `Err(_)` (timeout / activation failure) → 503.
pub fn classify_resolution(
    resolved: Result<Option<std::path::PathBuf>, ActivationError>,
) -> UpstreamChoice {
    match resolved {
        Ok(Some(socket)) => UpstreamChoice::Uds(socket),
        Ok(None) => UpstreamChoice::Unavailable,
        Err(_) => UpstreamChoice::Unavailable,
    }
}

/// Per-request context carried through the proxy phases.
#[derive(Default)]
pub struct RequestCtx {
    /// Service name the `Host` resolved to (for access logging).
    pub service_name: Option<String>,
    /// When set, `upstream_peer` proxies to the control backend (challenge).
    pub to_control_backend: bool,
}

/// The Denia L7 proxy. Shares `Arc<IngressState>` with the control plane.
pub struct DeniaProxy {
    state: Arc<IngressState>,
    control_backend: SocketAddr,
    /// Whether this instance is the `:80` listener (challenge interception +
    /// HTTP→HTTPS redirect run only here; `:443` falls straight through).
    is_http: bool,
    /// The configured control domain (e.g. `denia.example.com`), if any.
    /// Requests for this host are routed directly to `control_backend`.
    control_domain: Option<String>,
    /// Whether the control domain is TLS-enabled (used for redirect decisions
    /// on `:80` when the host matches `control_domain`).
    control_tls: bool,
}

impl DeniaProxy {
    /// Build a `:80` proxy (challenge interception + redirect enabled).
    pub fn http(
        state: Arc<IngressState>,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self {
            state,
            control_backend,
            is_http: true,
            control_domain,
            control_tls,
        }
    }

    /// Build a `:443` proxy (TLS already terminated by the listener; no
    /// challenge/redirect special-casing).
    pub fn https(
        state: Arc<IngressState>,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self {
            state,
            control_backend,
            is_http: false,
            control_domain,
            control_tls,
        }
    }
}

/// Whether `host` is the configured control domain (exact, already-lowercased
/// match; both sides are lowercased at their sources — `request_host` lowercases
/// the request Host, config lowercases `control_domain`).
pub fn is_control_host(host: &str, control_domain: Option<&str>) -> bool {
    control_domain == Some(host)
}

/// The effective `tls_for_host` fed to [`classify_port80`]: the control domain
/// uses `control_tls`; everything else uses its route's tls flag.
pub fn control_tls_for_host(
    host: &str,
    control_domain: Option<&str>,
    control_tls: bool,
    route_tls: Option<bool>,
) -> Option<bool> {
    if is_control_host(host, control_domain) {
        Some(control_tls)
    } else {
        route_tls
    }
}

/// Extract the request `Host` (lowercased, port stripped) from a Pingora
/// session. Prefers the `Host` header, falling back to the URI authority
/// (HTTP/2). Returns an empty string when absent.
fn request_host(session: &Session) -> String {
    let req = session.req_header();
    if let Some(value) = req.headers.get("host")
        && let Ok(s) = value.to_str()
    {
        return strip_port(s).to_ascii_lowercase();
    }
    if let Some(authority) = req.uri.authority() {
        return strip_port(authority.as_str()).to_ascii_lowercase();
    }
    String::new()
}

/// The `X-Forwarded-For` value for a proxied request: the client IP only (no
/// port). Overwriting with this (not appending) prevents a downstream client
/// from spoofing the value the loopback-trusting rate limiter keys on.
fn forwarded_for(client: Option<std::net::SocketAddr>) -> Option<String> {
    client.map(|addr| addr.ip().to_string())
}

/// Strip a trailing `:port` from a host authority. IPv6 literals are returned
/// verbatim (Denia only routes DNS names; bracketed literals never match a
/// validated route key anyway).
fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        return host;
    }
    match host.rsplit_once(':') {
        Some((h, _port)) => h,
        None => host,
    }
}

/// Request path (no query string) from the session.
fn request_path(session: &Session) -> String {
    session.req_header().uri.path().to_string()
}

#[async_trait]
impl ProxyHttp for DeniaProxy {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        if !self.is_http {
            // :443 — TLS terminated by the listener; route in upstream_peer.
            return Ok(false);
        }

        let path = request_path(session);
        let host = request_host(session);
        let route_tls = self.state.routes().resolve(&host).map(|r| r.tls);
        let tls_for_host = control_tls_for_host(
            &host,
            self.control_domain.as_deref(),
            self.control_tls,
            route_tls,
        );

        match classify_port80(&path, &host, tls_for_host) {
            Port80Decision::ToControlBackend => {
                ctx.to_control_backend = true;
                Ok(false)
            }
            Port80Decision::Redirect(location) => {
                let mut resp = ResponseHeader::build(308, Some(1))
                    .map_err(|_| Error::new(ErrorType::InternalError))?;
                resp.insert_header("Location", &location)
                    .map_err(|_| Error::new(ErrorType::InternalError))?;
                resp.insert_header("Content-Length", "0")
                    .map_err(|_| Error::new(ErrorType::InternalError))?;
                session.write_response_header(Box::new(resp), true).await?;
                Ok(true)
            }
            Port80Decision::Passthrough => Ok(false),
        }
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        // Challenge paths bypass host routing and proxy to the control plane.
        if ctx.to_control_backend {
            return Ok(Box::new(HttpPeer::new(
                self.control_backend,
                false,
                self.control_backend.ip().to_string(),
            )));
        }

        let host = request_host(session);

        if is_control_host(&host, self.control_domain.as_deref()) {
            return Ok(Box::new(HttpPeer::new(
                self.control_backend,
                false,
                self.control_backend.ip().to_string(),
            )));
        }

        // Resolve the Host to its route, capturing BOTH the pool lookup key
        // (`service_id` = `service.id.to_string()`, what `add_replica` keys by
        // and the activator parses as a UUID) and the human `service_name` (for
        // access logging only). Keying the pool by `service_name` was the C1
        // BLOCKER: every request missed the pool and 503'd.
        let (service_key, service_name) = match self.state.routes().resolve(&host) {
            Some(route) => (route.service_id.clone(), route.service_name.clone()),
            None => {
                // Unknown host → 404.
                session.respond_error(404).await?;
                return Err(Error::new(ErrorType::HTTPStatus(404)));
            }
        };
        ctx.service_name = Some(service_name);

        // Unauthenticated cold-start trigger (audit B2/A3): a request on the
        // public `:80`/`:443` can wake any scaled-to-zero *routed* service. This
        // is bounded by the per-service single-flight gate (one activation for N
        // concurrent waiters) and `ACTIVATION_WAIT`, so it cannot fan out into
        // unbounded launches. Cross-service abuse rate-limiting is intentionally
        // out of scope (documented in ADR-020); do NOT add a rate limiter here.
        let resolved = self.state.resolve_or_activate(&service_key).await;
        match classify_resolution(resolved) {
            UpstreamChoice::Uds(socket) => {
                let peer = HttpPeer::new_uds(&socket.to_string_lossy(), false, host)
                    .map_err(|_| Error::new(ErrorType::InternalError))?;
                Ok(Box::new(peer))
            }
            UpstreamChoice::Unavailable => {
                session.respond_error(503).await?;
                Err(Error::new(ErrorType::HTTPStatus(503)))
            }
            // ControlBackend/NotFound are handled above; keep exhaustive.
            UpstreamChoice::ControlBackend => Ok(Box::new(HttpPeer::new(
                self.control_backend,
                false,
                self.control_backend.ip().to_string(),
            ))),
            UpstreamChoice::NotFound => {
                session.respond_error(404).await?;
                Err(Error::new(ErrorType::HTTPStatus(404)))
            }
        }
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        let client = session.client_addr().and_then(|a| a.as_inet()).copied();
        if let Some(value) = forwarded_for(client) {
            // Overwrite (not append): strip any client-supplied X-Forwarded-For
            // so the rate-limit key cannot be spoofed.
            let _ = upstream_request.insert_header("X-Forwarded-For", &value);
        }
        Ok(())
    }

    async fn logging(&self, session: &mut Session, _e: Option<&Error>, ctx: &mut Self::CTX) {
        // Only log requests that resolved to a workload service (challenge /
        // control-backend traffic is logged by axum). This also avoids logging
        // under a synthetic key.
        let Some(service_name) = ctx.service_name.clone() else {
            return;
        };
        let entry = build_access_entry(
            service_name,
            session.req_header().method.as_str(),
            session.req_header().uri.path(),
            session
                .response_written()
                .map(|r| r.status.as_u16())
                .unwrap_or(0),
            Some(session.body_bytes_sent() as u64),
        );
        self.state.access_log().append(entry);
    }
}

/// Build an [`AccessEntry`] from the request fields captured in `logging()`.
///
/// Extracted as a free function so access-log fidelity (ADR-009: status, bytes,
/// host/path, method) is unit-tested without a live `Session`. The path is run
/// through [`sanitize_path`] so UUIDs / tokens are redacted (no secret-bearing
/// path segments are stored), and no request headers are recorded.
pub fn build_access_entry(
    service_name: String,
    method: &str,
    path: &str,
    status: u16,
    bytes: Option<u64>,
) -> AccessEntry {
    AccessEntry {
        service_name,
        method: method.to_string(),
        path: sanitize_path(path),
        status,
        bytes,
        duration_ms: None,
        recorded_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    #[test]
    fn acme_challenge_path_wins_even_for_unknown_host() {
        // No matching route (tls_for_host = None) but the challenge path must
        // still route to the control backend, before any 404.
        let d = classify_port80("/.well-known/acme-challenge/abc", "nope.example.com", None);
        assert_eq!(d, Port80Decision::ToControlBackend);
    }

    #[test]
    fn denia_challenge_path_wins_even_for_tls_host() {
        // Challenge interception precedes the HTTP->HTTPS redirect.
        let d = classify_port80(
            "/.well-known/denia-challenge/tok",
            "api.example.com",
            Some(true),
        );
        assert_eq!(d, Port80Decision::ToControlBackend);
    }

    #[test]
    fn tls_host_redirects_to_https() {
        let d = classify_port80("/dashboard?x=1", "api.example.com", Some(true));
        assert_eq!(
            d,
            Port80Decision::Redirect("https://api.example.com/dashboard?x=1".to_string())
        );
    }

    #[test]
    fn plain_host_passes_through() {
        let d = classify_port80("/", "plain.example.com", Some(false));
        assert_eq!(d, Port80Decision::Passthrough);
    }

    #[test]
    fn unknown_host_non_challenge_passes_through_to_404_path() {
        // Unknown host on a non-challenge path falls through; upstream_peer
        // then issues the 404.
        let d = classify_port80("/", "nope.example.com", None);
        assert_eq!(d, Port80Decision::Passthrough);
    }

    #[test]
    fn resolution_some_socket_is_uds() {
        let p = std::path::PathBuf::from("/run/denia/a.sock");
        assert_eq!(
            classify_resolution(Ok(Some(p.clone()))),
            UpstreamChoice::Uds(p)
        );
    }

    #[test]
    fn resolution_none_is_unavailable_503() {
        assert_eq!(classify_resolution(Ok(None)), UpstreamChoice::Unavailable);
    }

    #[test]
    fn resolution_timeout_is_unavailable_503() {
        assert_eq!(
            classify_resolution(Err(ActivationError::Timeout)),
            UpstreamChoice::Unavailable
        );
    }

    #[test]
    fn resolution_failure_is_unavailable_503() {
        assert_eq!(
            classify_resolution(Err(ActivationError::Failed("boom".into()))),
            UpstreamChoice::Unavailable
        );
    }

    #[test]
    fn forwarded_for_uses_client_ip_only() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let client = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 54321);
        assert_eq!(forwarded_for(Some(client)).as_deref(), Some("203.0.113.7"));
        assert_eq!(forwarded_for(None), None);
    }

    #[test]
    fn strip_port_removes_trailing_port() {
        assert_eq!(strip_port("api.example.com:8080"), "api.example.com");
        assert_eq!(strip_port("api.example.com"), "api.example.com");
        assert_eq!(strip_port("[::1]"), "[::1]");
    }

    #[test]
    fn build_access_entry_sanitizes_path_and_records_status_bytes() {
        let entry = build_access_entry(
            "api".to_string(),
            "GET",
            "/users/a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            200,
            Some(1234),
        );
        assert_eq!(entry.service_name, "api");
        assert_eq!(entry.method, "GET");
        assert_eq!(entry.path, "/users/{id}");
        assert_eq!(entry.status, 200);
        assert_eq!(entry.bytes, Some(1234));
        assert!(!entry.recorded_at.is_empty());
    }

    #[test]
    fn is_control_host_matches_exact_lowercased() {
        assert!(is_control_host(
            "denia.example.com",
            Some("denia.example.com")
        ));
        assert!(!is_control_host(
            "other.example.com",
            Some("denia.example.com")
        ));
        assert!(!is_control_host("denia.example.com", None));
    }

    #[test]
    fn control_tls_for_host_overrides_route_lookup() {
        assert_eq!(
            control_tls_for_host("denia.example.com", Some("denia.example.com"), true, None),
            Some(true)
        );
        assert_eq!(
            control_tls_for_host("denia.example.com", Some("denia.example.com"), false, None),
            Some(false)
        );
        assert_eq!(
            control_tls_for_host(
                "svc.example.com",
                Some("denia.example.com"),
                true,
                Some(true)
            ),
            Some(true)
        );
        assert_eq!(
            control_tls_for_host("nope.example.com", Some("denia.example.com"), true, None),
            None
        );
    }
}
