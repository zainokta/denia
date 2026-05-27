//! In-process Pingora L7 ingress (ADR-020).
//!
//! This module replaces the supervised-Traefik + loopback-bridge ingress with
//! an in-process [Pingora](https://github.com/cloudflare/pingora) proxy that
//! binds `:80`/`:443`, terminates TLS using certificates Denia issues itself via
//! ACME, and proxies directly to workload Unix sockets.
//!
//! It is being introduced additively alongside the existing Traefik/bridge code;
//! the old modules are removed in a later cutover chunk.

pub mod acme;
pub mod proxy;
pub mod server;
pub mod state;
pub mod tls;

pub use acme::{
    AcmeDriver, AcmeError, ChallengeStore, IssuedCert, RENEWAL_WINDOW_DAYS, load_certs_from_disk,
    persist_cert, select_renewals,
};
pub use proxy::{DeniaProxy, Port80Decision, RequestCtx, UpstreamChoice, classify_port80};
pub use server::{IngressServerConfig, ServerBuildError, build_server, run_server};
pub use state::{
    ActivationError, ActivationHook, CertStore, IngressError, IngressState, ParsedCert,
    ReplicaEndpoint, RouteSpec, RouteTable,
};
pub use tls::{DeniaCertResolver, resolve_sni_cert};
