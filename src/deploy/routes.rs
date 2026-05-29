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

/// Boot-time route table rebuild.
///
/// On a fresh daemon process the in-memory route snapshot (`SharedRoutes`) and
/// the live `IngressState` route table both start empty. The two boot launch
/// paths repopulate only the replica *pools*: plain services via
/// `autostart_plain_promoted` (which re-runs `write_route_table`) and autoscaled
/// services via the controller's `reconcile_boot`/`activate` (which do NOT touch
/// routes). Without the Host -> `service_id` route, an autoscaled service with a
/// verified domain answers 404 until it is redeployed, and a scale-to-zero
/// service can never be woken — the proxy can't resolve the Host to reach the
/// activation hook.
///
/// This seeds the snapshot from SQLite for every service that "should be
/// running" — it has a promoted deployment (an explicit stop clears that row)
/// AND at least one verified hostname — then swaps the whole table into the live
/// ingress state. Unlike [`apply_routes`] it does NOT require a pre-existing
/// snapshot entry (boot is exactly the no-entry case). Single control-plane
/// writer, whole-table last-writer-wins swap (audit A8).
pub fn rebuild_routes_on_boot(state: &crate::app::AppState) -> Result<(), DeployError> {
    let services = state.services.list_services()?;
    let mut snapshot = state
        .routes
        .lock()
        .map_err(|_| DeployError::RoutesLockPoisoned)?;

    for svc in services {
        // Skip services that should not be running: never deployed, or
        // explicitly stopped (stop clears the promoted row).
        if state.deployments.promoted_deployment(svc.id)?.is_none() {
            continue;
        }
        let hostnames = state.domains.list_verified_hostnames(svc.id)?;
        if hostnames.is_empty() {
            continue;
        }
        let route_key = svc.id.to_string();
        snapshot.insert(
            route_key.clone(),
            RouteSpec {
                route_key: format!("svc-{}", svc.id),
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

#[cfg(test)]
mod tests {
    use crate::app::AppState;
    use crate::config::AppConfig;
    use crate::domain::{
        DeploymentRequest, DomainStatus, ExternalImageSource, HealthCheck, ResourceLimits,
        ServiceConfig, ServiceDomain, ServiceSource,
    };
    use crate::state::SqliteStore;
    use uuid::Uuid;

    fn seed_service(store: &SqliteStore) -> ServiceConfig {
        let project_id = store.default_project_id().expect("default project");
        store
            .put_service(
                ServiceConfig::new(
                    project_id,
                    "web",
                    vec!["placeholder.example.test".to_string()],
                    ServiceSource::ExternalImage(ExternalImageSource {
                        image: "ghcr.io/acme/web:latest".to_string(),
                        credential: None,
                        registry_id: None,
                        image_ref: None,
                    }),
                    3000,
                    HealthCheck::new("/ready", 5),
                    Some(ResourceLimits::default()),
                    vec![],
                )
                .expect("service"),
            )
            .expect("stored service")
    }

    fn promote_new_deployment(store: &SqliteStore, service_id: Uuid) {
        let deployment = store
            .create_deployment(DeploymentRequest::external_image(
                service_id,
                "ghcr.io/acme/web:latest",
            ))
            .expect("deployment");
        store
            .promote_deployment(service_id, deployment.id)
            .expect("promote");
    }

    fn add_domain(store: &SqliteStore, service_id: Uuid, hostname: &str, verified: bool) {
        let now = chrono::Utc::now();
        let d = ServiceDomain {
            id: Uuid::now_v7(),
            service_id,
            hostname: hostname.to_string(),
            status: DomainStatus::Pending,
            challenge_token: format!("token-{hostname}"),
            verified_at: None,
            last_check_at: None,
            last_error: None,
            created_at: now,
        };
        store.put_service_domain(&d).expect("put domain");
        if verified {
            store
                .update_service_domain_status(d.id, DomainStatus::Verified, Some(now), None)
                .expect("verify domain");
        }
    }

    #[test]
    fn routes_promoted_service_with_verified_domain() {
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let svc = seed_service(&store);
        promote_new_deployment(&store, svc.id);
        add_domain(&store, svc.id, "app.example.com", true);

        let state = AppState::new(AppConfig::for_test("test-token"), &store);
        super::rebuild_routes_on_boot(&state).expect("rebuild");

        assert_eq!(
            state.ingress.resolve_host("app.example.com"),
            Some(svc.name.clone())
        );
        assert_eq!(
            state
                .ingress
                .routes()
                .resolve("app.example.com")
                .map(|r| r.service_id.clone()),
            Some(svc.id.to_string())
        );
    }

    #[test]
    fn skips_service_without_promoted_deployment() {
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let svc = seed_service(&store);
        // Verified domain but never deployed (no promoted row).
        add_domain(&store, svc.id, "app.example.com", true);

        let state = AppState::new(AppConfig::for_test("test-token"), &store);
        super::rebuild_routes_on_boot(&state).expect("rebuild");

        assert_eq!(state.ingress.resolve_host("app.example.com"), None);
    }

    #[test]
    fn skips_service_without_verified_domain() {
        let store = SqliteStore::open_in_memory().expect("sqlite");
        store.migrate().expect("migrate");
        let svc = seed_service(&store);
        promote_new_deployment(&store, svc.id);
        // Domain attached but still pending (unverified).
        add_domain(&store, svc.id, "app.example.com", false);

        let state = AppState::new(AppConfig::for_test("test-token"), &store);
        super::rebuild_routes_on_boot(&state).expect("rebuild");

        assert_eq!(state.ingress.resolve_host("app.example.com"), None);
    }
}
