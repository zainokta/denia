use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::deploy::error::DeployError;
use crate::traefik::{IngressRenderOptions, RouteSpec, render_file_provider_config};

pub type SharedRoutes = Arc<Mutex<BTreeMap<String, RouteSpec>>>;

pub fn default_ingress_options() -> IngressRenderOptions {
    IngressRenderOptions {
        acme_resolver: String::new(),
        control_domain: None,
        control_tls: false,
        control_backend_addr: String::new(),
    }
}

pub fn rerender_traefik(state: &crate::app::AppState) -> Result<(), DeployError> {
    let services = state.services.list_services()?;
    let mut routes_guard = state
        .routes
        .lock()
        .map_err(|_| DeployError::BridgeLockPoisoned)?;
    let existing = routes_guard.clone();
    routes_guard.clear();
    for svc in services {
        let hostnames = state.domains.list_verified_hostnames(svc.id)?;
        if hostnames.is_empty() {
            continue;
        }
        let Some(prev) = existing.get(&svc.name) else {
            // Service has never been routed (no bridge_port known). Stays unrouted.
            continue;
        };
        routes_guard.insert(
            svc.name.clone(),
            RouteSpec {
                route_key: prev.route_key.clone(),
                service_name: svc.name.clone(),
                domains: hostnames,
                bridge_port: prev.bridge_port,
                tls: svc.tls_enabled,
            },
        );
    }
    let yaml = render_file_provider_config(
        &routes_guard.values().cloned().collect::<Vec<_>>(),
        &state.ingress_options,
    )?;
    if let Some(parent) = state.config.traefik_dynamic_config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&state.config.traefik_dynamic_config_path, yaml)?;
    Ok(())
}
