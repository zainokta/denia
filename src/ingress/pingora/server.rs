//! Pingora `Server` assembly and lifecycle.
//!
//! Builds the Pingora `Server` that hosts the `:80` and `:443` proxy services,
//! both sharing `Arc<IngressState>` with the control plane. The server runs on
//! a dedicated OS thread (Spike 0.1) and is driven via an injected shutdown
//! signal so Denia keeps signal ownership (wired in `main` — Chunk C).
//!
//! - `:80` (`DeniaProxy::http`): challenge interception + HTTP→HTTPS redirect +
//!   plain-HTTP proxying.
//! - `:443` (`DeniaProxy::https`): TLS terminated by a `TlsAccept` callback
//!   ([`DeniaCertResolver`]) that serves the per-SNI cert from `CertStore`.

use std::net::SocketAddr;
use std::sync::Arc;

use pingora::listeners::tls::TlsSettings;
use pingora::server::Server;
use pingora_proxy::http_proxy_service;

use super::proxy::DeniaProxy;
use super::state::IngressState;
use super::tls::DeniaCertResolver;

/// Configuration for the Pingora ingress server.
///
/// Ports default to `80`/`443`; the control backend is the axum bind address
/// used for ACME / `denia-challenge` proxying and the control domain.
#[derive(Debug, Clone)]
pub struct IngressServerConfig {
    /// Address the `:80` (`web`) listener binds to.
    pub http_addr: SocketAddr,
    /// Address the `:443` (`websecure`) listener binds to.
    pub https_addr: SocketAddr,
    /// Control-plane backend (axum) address, e.g. `127.0.0.1:7180`.
    pub control_backend: SocketAddr,
}

impl IngressServerConfig {
    /// Test/default configuration binding loopback ports.
    #[cfg(test)]
    pub fn test_defaults() -> Self {
        Self {
            http_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            https_addr: SocketAddr::from(([127, 0, 0, 1], 8443)),
            control_backend: SocketAddr::from(([127, 0, 0, 1], 7180)),
        }
    }
}

/// Error building the Pingora ingress server (e.g. TLS settings construction).
#[derive(Debug, thiserror::Error)]
pub enum ServerBuildError {
    #[error("failed to construct pingora server: {0}")]
    Server(String),
    #[error("failed to construct TLS settings: {0}")]
    Tls(String),
}

/// Build the Pingora `Server` hosting the `:80` and `:443` proxy services.
///
/// The returned `Server` is not yet started; the caller runs it on a dedicated
/// OS thread via `Server::run(RunArgs { shutdown_signal, .. })` (never
/// `run_forever()`), so Denia's signal handling stays authoritative (Chunk C).
///
/// `:443` is wired with the dynamic per-SNI [`DeniaCertResolver`] callback;
/// certs must be boot-loaded into `IngressState` before this listener accepts
/// connections (handled in `main`, Chunk C).
pub fn build_server(
    state: Arc<IngressState>,
    cfg: &IngressServerConfig,
) -> Result<Server, ServerBuildError> {
    let mut server = Server::new(None).map_err(|e| ServerBuildError::Server(e.to_string()))?;
    server.bootstrap();
    let conf = server.configuration.clone();

    // :80 — challenge interception, redirect, plain-HTTP proxying.
    let mut http_service =
        http_proxy_service(&conf, DeniaProxy::http(state.clone(), cfg.control_backend));
    http_service.add_tcp(&cfg.http_addr.to_string());
    server.add_service(http_service);

    // :443 — TLS terminated via the dynamic per-SNI cert callback.
    let mut https_service =
        http_proxy_service(&conf, DeniaProxy::https(state.clone(), cfg.control_backend));
    let resolver = Box::new(DeniaCertResolver::new(state));
    let tls_settings =
        TlsSettings::with_callbacks(resolver).map_err(|e| ServerBuildError::Tls(e.to_string()))?;
    https_service.add_tls_with_settings(&cfg.https_addr.to_string(), None, tls_settings);
    server.add_service(https_service);

    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_server_constructs_http_and_tls_services() {
        // Smoke test: the server (with the `:443` TLS callback wired) builds
        // without panicking. We do not run it (that binds real ports and is
        // covered by the privileged end-to-end test in Phase 8).
        let state = Arc::new(IngressState::default());
        let _server =
            build_server(state, &IngressServerConfig::test_defaults()).expect("server builds");
    }
}
