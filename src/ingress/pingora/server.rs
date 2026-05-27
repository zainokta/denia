//! Pingora `Server` assembly and lifecycle.
//!
//! Builds the Pingora `Server` that hosts the `:80` (and later `:443`) proxy
//! services. The server runs on a dedicated OS thread (see Spike 0.1) and is
//! driven via an injected shutdown signal so Denia keeps signal ownership.
//!
//! This is the additive skeleton: the proxy is a compiling placeholder until
//! `DeniaProxy` (`ProxyHttp`) lands in Phase 3 and `IngressState` is wired in.

use std::net::SocketAddr;

use async_trait::async_trait;
use pingora::server::Server;
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType};
use pingora_proxy::{ProxyHttp, Session, http_proxy_service};

/// Configuration for the Pingora ingress server.
///
/// Ports default to `80`/`443`; the control backend is the axum bind address
/// used for ACME / `denia-challenge` proxying and the control domain.
#[derive(Debug, Clone)]
pub struct IngressServerConfig {
    /// Address the `:80` (`web`) listener binds to.
    pub http_addr: SocketAddr,
    /// Address the `:443` (`websecure`) listener binds to (wired in Phase 4).
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

/// Placeholder L7 proxy. Replaced by the real routing brain in Phase 3.
///
/// Currently it forwards every request to the control backend so the skeleton
/// compiles and binds; route resolution, scale-from-zero and TLS land later.
#[allow(dead_code)]
struct DeniaProxy {
    control_backend: SocketAddr,
}

#[async_trait]
impl ProxyHttp for DeniaProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        // Placeholder: route everything to the control backend until Phase 3
        // implements host resolution and replica selection.
        Err(Error::explain(
            ErrorType::InternalError,
            "denia ingress proxy not yet wired (skeleton)",
        ))
    }
}

/// Build the Pingora `Server` hosting the `:80` proxy service.
///
/// The returned `Server` is not yet started; the caller runs it on a dedicated
/// OS thread via `Server::run(RunArgs { shutdown_signal, .. })` (never
/// `run_forever()`), so Denia's signal handling stays authoritative.
///
/// The `:443` TLS service and `IngressState` wiring are added in later tasks.
#[allow(dead_code)]
pub fn build_server(cfg: &IngressServerConfig) -> Server {
    let mut server = Server::new(None).expect("build pingora server");
    server.bootstrap();

    let conf = server.configuration.clone();
    let mut http_service = http_proxy_service::<DeniaProxy>(
        &conf,
        DeniaProxy {
            control_backend: cfg.control_backend,
        },
    );
    http_service.add_tcp(&cfg.http_addr.to_string());
    server.add_service(http_service);

    server
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_server_constructs_with_http_service() {
        // Smoke test: the skeleton server builds without panicking. We do not
        // run it (that binds real ports and is covered by the privileged
        // end-to-end test in Phase 8).
        let _server = build_server(&IngressServerConfig::test_defaults());
    }
}
