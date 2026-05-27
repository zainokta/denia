use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::deploy::error::DeployError;
use crate::ingress::pingora::{RouteSpec, RouteTable};

/// Snapshot of the live route table, keyed by `service.id` (F-3: names are only
/// unique per project), surfaced by `GET /v1/ingress/routes`. The canonical
/// routing source is the `Arc<IngressState>` route table; this map mirrors it so
/// the read-only API does not need to reach into the proxy state.
pub type SharedRoutes = Arc<Mutex<BTreeMap<String, RouteSpec>>>;

/// Build a [`RouteTable`] from a snapshot map. Each entry is inserted via
/// [`RouteTable::try_upsert`] so an invalid domain surfaces as
/// [`crate::ingress::pingora::IngressError`] rather than being silently dropped
/// (audit A1).
pub fn route_table_from_snapshot(
    snapshot: &BTreeMap<String, RouteSpec>,
) -> Result<RouteTable, DeployError> {
    let mut table = RouteTable::default();
    for spec in snapshot.values() {
        if spec.domains.is_empty() {
            // A service with a bridge entry but no verified domains has nothing
            // to route yet; skip it rather than erroring.
            continue;
        }
        table.try_upsert(spec.clone())?;
    }
    Ok(table)
}

/// Rebuild the live route table from persisted state and apply it to the shared
/// `IngressState` (and the API snapshot).
///
/// Routes are keyed by `service.id` (globally unique) — not `service.name`,
/// which is only unique within a project (F-3) — so two projects' same-named
/// services cannot collide. Each service's verified hostnames are looked up and,
/// when present, upserted into the table via [`RouteTable::try_upsert`] so an
/// invalid domain is surfaced as a typed [`DeployError::Ingress`] at this
/// control-plane boundary (audit A1).
///
/// The `IngressState` route table is the single writer's last-writer-wins
/// snapshot (audit A8): only the control plane (deploy/verify/delete paths) calls
/// this, so a whole-table swap is safe.
pub fn apply_routes(state: &crate::app::AppState) -> Result<(), DeployError> {
    let services = state.services.list_services()?;
    let mut snapshot = state
        .routes
        .lock()
        .map_err(|_| DeployError::RoutesLockPoisoned)?;

    // Rebuild the snapshot from current DB state, preserving each service's
    // existing entry (and its tls flag) while refreshing verified hostnames.
    let existing = snapshot.clone();
    snapshot.clear();
    for svc in services {
        let route_key = svc.id.to_string();
        let hostnames = state.domains.list_verified_hostnames(svc.id)?;
        if hostnames.is_empty() {
            continue;
        }
        // A service is only routable once it has been deployed (an entry exists).
        let Some(prev) = existing.get(&route_key) else {
            continue;
        };
        snapshot.insert(
            route_key.clone(),
            RouteSpec {
                route_key: prev.route_key.clone(),
                service_name: svc.name.clone(),
                // Proxy pool lookup key = service.id.to_string(), matching the
                // `add_replica` key so Host -> route.service_id -> pool hit (C1).
                service_id: route_key,
                domains: hostnames,
                tls: svc.tls_enabled,
            },
        );
    }

    let table = route_table_from_snapshot(&snapshot)?;
    state.ingress.swap_routes(table);
    Ok(())
}
