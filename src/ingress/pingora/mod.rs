//! In-process Pingora L7 ingress (ADR-020).
//!
//! This module replaces the supervised-Traefik + loopback-bridge ingress with
//! an in-process [Pingora](https://github.com/cloudflare/pingora) proxy that
//! binds `:80`/`:443`, terminates TLS using certificates Denia issues itself via
//! ACME, and proxies directly to workload Unix sockets.
//!
//! It is being introduced additively alongside the existing Traefik/bridge code;
//! the old modules are removed in a later cutover chunk.

pub mod server;
pub mod state;

pub use server::{IngressServerConfig, build_server};
pub use state::{
    ActivationError, ActivationHook, CertStore, IngressError, IngressState, ParsedCert,
    ReplicaEndpoint, RouteSpec, RouteTable,
};
