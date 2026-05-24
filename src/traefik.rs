use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    pub service_name: String,
    pub domains: Vec<String>,
    pub bridge_port: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TraefikError {
    #[error("route service name cannot be empty")]
    EmptyServiceName,
    #[error("route must include at least one domain")]
    MissingDomain,
    #[error("bridge port must be greater than zero")]
    InvalidBridgePort,
}

pub fn render_file_provider_config(routes: &[RouteSpec]) -> Result<String, TraefikError> {
    let mut output = String::from("http:\n  routers:\n");
    for route in routes {
        validate(route)?;
        let rule = route
            .domains
            .iter()
            .map(|domain| format!("Host(`{domain}`)"))
            .collect::<Vec<_>>()
            .join(" || ");
        output.push_str(&format!(
            "    {name}:\n      rule: \"{rule}\"\n      service: {name}\n",
            name = route.service_name
        ));
    }

    output.push_str("  services:\n");
    for route in routes {
        output.push_str(&format!(
            "    {name}:\n      loadBalancer:\n        servers:\n          - url: \"http://127.0.0.1:{port}\"\n",
            name = route.service_name,
            port = route.bridge_port
        ));
    }
    Ok(output)
}

fn validate(route: &RouteSpec) -> Result<(), TraefikError> {
    if route.service_name.trim().is_empty() {
        return Err(TraefikError::EmptyServiceName);
    }
    if route.domains.is_empty() {
        return Err(TraefikError::MissingDomain);
    }
    if route.bridge_port == 0 {
        return Err(TraefikError::InvalidBridgePort);
    }
    Ok(())
}
