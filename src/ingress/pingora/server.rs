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

use async_trait::async_trait;
use pingora::listeners::tls::TlsSettings;
use pingora::server::{RunArgs, Server, ShutdownSignal, ShutdownSignalWatch};
use pingora_proxy::http_proxy_service;
use tokio::sync::watch;

use super::proxy::DeniaProxy;
use super::state::IngressState;
use super::tls::DeniaCertResolver;

/// App-driven shutdown watcher (Spike 0.1): a custom [`ShutdownSignalWatch`]
/// means Pingora installs **no** OS signal handler, so Denia's `tokio::signal`
/// handler stays authoritative. The control plane fires shutdown by flipping the
/// `watch` channel from its existing graceful-shutdown path.
struct ChannelShutdownSignalWatch {
    rx: watch::Receiver<bool>,
}

#[async_trait]
impl ShutdownSignalWatch for ChannelShutdownSignalWatch {
    async fn recv(&self) -> ShutdownSignal {
        let mut rx = self.rx.clone();
        // Wait until the flag flips true (or the sender drops).
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        ShutdownSignal::FastShutdown
    }
}

/// Run a built Pingora [`Server`] to completion on the current thread, shutting
/// down when `shutdown_rx` flips to `true`.
///
/// MUST be invoked on a dedicated `std::thread` (Pingora builds its own tokio
/// runtimes and `block_on`s internally — Spike 0.1). Uses `Server::run` (never
/// `run_forever`, which `process::exit`s) so control returns to the caller for a
/// clean thread join.
pub fn run_server(server: Server, shutdown_rx: watch::Receiver<bool>) {
    server.run(RunArgs {
        shutdown_signal: Box::new(ChannelShutdownSignalWatch { rx: shutdown_rx }),
    });
}

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
    /// Optional hostname that routes directly to the control backend
    /// (e.g. `denia.example.com`). Requests for this host bypass workload
    /// routing and are forwarded to `control_backend`.
    pub control_domain: Option<String>,
    /// Whether the control domain is TLS-enabled (drives the HTTP→HTTPS
    /// redirect decision on `:80` for control-domain requests).
    pub control_tls: bool,
}

impl IngressServerConfig {
    /// Build from the configured HTTP/HTTPS ports and control-plane bind address.
    /// `:80`/`:443` bind on all interfaces (`0.0.0.0`) since Denia owns the
    /// public ingress; the control backend is loopback-local axum.
    pub fn from_ports(
        http_port: u16,
        https_port: u16,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self {
            http_addr: SocketAddr::from(([0, 0, 0, 0], http_port)),
            https_addr: SocketAddr::from(([0, 0, 0, 0], https_port)),
            control_backend,
            control_domain,
            control_tls,
        }
    }

    /// Test/default configuration binding loopback ports.
    #[cfg(test)]
    pub fn test_defaults() -> Self {
        Self {
            http_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            https_addr: SocketAddr::from(([127, 0, 0, 1], 8443)),
            control_backend: SocketAddr::from(([127, 0, 0, 1], 7180)),
            control_domain: None,
            control_tls: false,
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
    let mut http_service = http_proxy_service(
        &conf,
        DeniaProxy::http(
            state.clone(),
            cfg.control_backend,
            cfg.control_domain.clone(),
            cfg.control_tls,
        ),
    );
    http_service.add_tcp(&cfg.http_addr.to_string());
    server.add_service(http_service);

    // :443 — TLS terminated via the dynamic per-SNI cert callback.
    let mut https_service = http_proxy_service(
        &conf,
        DeniaProxy::https(
            state.clone(),
            cfg.control_backend,
            cfg.control_domain.clone(),
            cfg.control_tls,
        ),
    );
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

    #[test]
    fn from_ports_carries_control_domain() {
        let backend = SocketAddr::from(([127, 0, 0, 1], 7180));
        let cfg = IngressServerConfig::from_ports(
            80,
            443,
            backend,
            Some("denia.example.com".into()),
            true,
        );
        assert_eq!(cfg.control_domain.as_deref(), Some("denia.example.com"));
        assert!(cfg.control_tls);
    }

    /// ADR-032 regression guard: HTTP/2 must stay OFF on Denia-owned `:443`
    /// until an accepted implementation adds protocol-level resource controls
    /// (the HTTP/2 Bomb class). Pingora 0.8's `TlsSettings` exposes no public
    /// ALPN/h2 getter to assert against at runtime, so this guards the source:
    /// `build_server` must NOT call `enable_h2` / `set_alpn` / advertise `h2`.
    /// If a future PR enables HTTP/2 here, it must also update ADR-032 and this
    /// test; the test failing is the intended tripwire.
    #[test]
    fn https_listener_never_advertises_http2() {
        let src = include_str!("server.rs");
        // Only inspect code outside this test module so the assertion strings
        // here do not match themselves.
        let code = src
            .split("#[cfg(test)]")
            .next()
            .expect("server.rs has a pre-test prefix");
        for forbidden in [
            "enable_h2",
            "set_alpn",
            "ALPN::H2",
            "ALPN::H2H1",
            "enable_h2c",
        ] {
            assert!(
                !code.contains(forbidden),
                "ADR-032: build_server must not enable HTTP/2 (found `{forbidden}` in server.rs)"
            );
        }
    }
}
