//! Shared ingress state for the Pingora proxy.
//!
//! `IngressState` is the control brain shared (via `Arc`) between the Denia
//! control plane and the Pingora proxy services running on a dedicated OS
//! thread. It absorbs the loopback bridge's replica pools, health, scale-from-
//! zero activation, idle tracking and access log, and adds an `ArcSwap`-backed
//! route table and cert store.
//!
//! NOTE: this is the additive Phase 2 home. The legacy `src/ingress/bridge.rs`
//! still owns the live transport during this chunk; the types here are a
//! parallel, distinct definition (no name collision via module paths).

use thiserror::Error;

/// Typed errors at the ingress boundary.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IngressError {
    #[error("route service name cannot be empty")]
    EmptyServiceName,
    #[error("route must include at least one domain")]
    MissingDomain,
    #[error("invalid domain: {0}")]
    InvalidDomain(String),
}

// The remaining types (`RouteSpec`, `RouteTable`, `IngressState`, `CertStore`,
// `ParsedCert`) are implemented in Tasks 2.1–2.3.
