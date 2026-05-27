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

use std::collections::BTreeMap;

use serde::Serialize;
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

/// A single service's routing entry.
///
/// This is the Pingora-era successor to `traefik::RouteSpec`. It drops the
/// `bridge_port` field — with UDS upstreams (Spike 0.2 = YES) there is no
/// loopback bridge port to render. `route_key` is the stable per-entry key
/// (the service id; see `coordinator.rs` F-3 comment) used to deduplicate
/// services whose names collide across projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteSpec {
    pub route_key: String,
    pub service_name: String,
    pub domains: Vec<String>,
    pub tls: bool,
}

/// Host-indexed routing table.
///
/// Each domain maps to the owning `RouteSpec`. The table is rebuilt and swapped
/// atomically (`ArcSwap`) on route changes, so resolution never blocks on a
/// lock in the proxy hot path.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    by_host: BTreeMap<String, RouteSpec>,
}

impl RouteTable {
    /// Insert or replace `spec`, indexing it under each of its domains.
    ///
    /// If a domain previously pointed at a different `route_key`, the most
    /// recent `upsert` wins for that host.
    pub fn upsert(&mut self, spec: RouteSpec) {
        for domain in &spec.domains {
            self.by_host.insert(domain.clone(), spec.clone());
        }
    }

    /// Remove every host entry owned by `route_key`.
    pub fn remove(&mut self, route_key: &str) {
        self.by_host.retain(|_, spec| spec.route_key != route_key);
    }

    /// Resolve a request `Host` to its owning route, if any.
    pub fn resolve(&self, host: &str) -> Option<&RouteSpec> {
        self.by_host.get(host)
    }

    /// Number of distinct host entries (for diagnostics/tests).
    pub fn host_count(&self) -> usize {
        self.by_host.len()
    }
}

#[cfg(test)]
mod route_table_tests {
    use super::*;

    #[test]
    fn route_table_resolves_host_to_service() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-1".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into()],
            tls: true,
        });
        assert_eq!(
            t.resolve("api.example.com").map(|r| r.service_name.as_str()),
            Some("api")
        );
        assert!(t.resolve("nope.example.com").is_none());
    }

    #[test]
    fn upsert_indexes_all_domains_and_remove_drops_them() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-1".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into(), "www.api.example.com".into()],
            tls: false,
        });
        assert_eq!(t.host_count(), 2);
        assert!(t.resolve("www.api.example.com").is_some());

        t.remove("svc-1");
        assert_eq!(t.host_count(), 0);
        assert!(t.resolve("api.example.com").is_none());
    }

    #[test]
    fn latest_upsert_wins_for_a_shared_host() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-old".into(),
            service_name: "old".into(),
            domains: vec!["app.example.com".into()],
            tls: false,
        });
        t.upsert(RouteSpec {
            route_key: "svc-new".into(),
            service_name: "new".into(),
            domains: vec!["app.example.com".into()],
            tls: true,
        });
        let resolved = t.resolve("app.example.com").expect("resolved");
        assert_eq!(resolved.service_name, "new");
        assert!(resolved.tls);
    }
}
