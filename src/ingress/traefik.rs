use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteSpec {
    pub route_key: String,
    pub service_name: String,
    pub domains: Vec<String>,
    pub bridge_port: u16,
    pub tls: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngressRenderOptions {
    pub acme_resolver: String,
    pub control_domain: Option<String>,
    pub control_tls: bool,
    pub control_backend_addr: String,
}

impl IngressRenderOptions {
    #[cfg(test)]
    pub fn test_defaults() -> Self {
        Self {
            acme_resolver: "le".to_string(),
            control_domain: None,
            control_tls: false,
            control_backend_addr: "http://127.0.0.1:7180".to_string(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TraefikError {
    #[error("route service name cannot be empty")]
    EmptyServiceName,
    #[error("route must include at least one domain")]
    MissingDomain,
    #[error("bridge port must be greater than zero")]
    InvalidBridgePort,
    #[error("tls enabled but acme resolver is empty")]
    MissingResolver,
    #[error("invalid domain: {0}")]
    InvalidDomain(String),
}

pub fn render_file_provider_config(
    routes: &[RouteSpec],
    options: &IngressRenderOptions,
) -> Result<String, TraefikError> {
    let mut has_tls = false;
    let mut output = String::from("http:\n");

    output.push_str("  routers:\n");
    for route in routes {
        validate(route)?;
        let rule = route
            .domains
            .iter()
            .map(|domain| format!("Host(`{domain}`)"))
            .collect::<Vec<_>>()
            .join(" || ");

        if route.tls {
            if options.acme_resolver.is_empty() {
                return Err(TraefikError::MissingResolver);
            }
            has_tls = true;
            let redirect_key = format!("{}-redirect", route.route_key);
            output.push_str(&format!(
                "    {redirect_key}:\n      rule: \"{rule}\"\n      entryPoints:\n        - web\n      middlewares:\n        - {redirect_key}-redirect\n      service: noop@internal\n"
            ));
            output.push_str(&format!(
                "    {key}:\n      rule: \"{rule}\"\n      entryPoints:\n        - websecure\n      tls:\n        certResolver: {resolver}\n      service: {key}\n",
                key = route.route_key,
                resolver = options.acme_resolver,
            ));
        } else {
            output.push_str(&format!(
                "    {key}:\n      rule: \"{rule}\"\n      entryPoints:\n        - web\n      service: {key}\n",
                key = route.route_key,
            ));
        }
    }

    if options
        .control_domain
        .as_ref()
        .is_some_and(|d| !d.is_empty())
    {
        let domain = options.control_domain.as_ref().unwrap();
        validate_domain(domain)?;
        let rule = format!("Host(`{domain}`)");
        if options.control_tls {
            if options.acme_resolver.is_empty() {
                return Err(TraefikError::MissingResolver);
            }
            output.push_str(&format!(
                "    denia-control-redirect:\n      rule: \"{rule}\"\n      entryPoints:\n        - web\n      middlewares:\n        - denia-control-redirect\n      service: noop@internal\n"
            ));
            output.push_str(&format!(
                "    denia-control:\n      rule: \"{rule}\"\n      entryPoints:\n        - websecure\n      tls:\n        certResolver: {resolver}\n      service: denia-control\n",
                resolver = options.acme_resolver,
            ));
        } else {
            output.push_str(&format!(
                "    denia-control:\n      rule: \"{rule}\"\n      entryPoints:\n        - web\n      service: denia-control\n",
            ));
        }
    }

    output.push_str(
        "    denia-challenge:\n      rule: \"PathPrefix(`/.well-known/denia-challenge`)\"\n      entryPoints:\n        - web\n      priority: 1000\n      service: denia-challenge\n",
    );

    if has_tls
        || options
            .control_domain
            .as_ref()
            .is_some_and(|d| !d.is_empty() && options.control_tls)
    {
        output.push_str("  middlewares:\n");
        for route in routes.iter().filter(|r| r.tls) {
            output.push_str(&format!(
                "    {key}-redirect:\n      redirectScheme:\n        scheme: https\n        permanent: true\n",
                key = route.route_key,
            ));
        }
        if options
            .control_domain
            .as_ref()
            .is_some_and(|d| !d.is_empty())
            && options.control_tls
        {
            output.push_str("    denia-control-redirect:\n      redirectScheme:\n        scheme: https\n        permanent: true\n");
        }
    }

    output.push_str("  services:\n");
    for route in routes {
        output.push_str(&format!(
            "    {key}:\n      loadBalancer:\n        servers:\n          - url: \"http://127.0.0.1:{port}\"\n",
            key = route.route_key,
            port = route.bridge_port,
        ));
    }

    if let Some(ref domain) = options.control_domain
        && !domain.is_empty()
    {
        output.push_str(&format!(
                "    denia-control:\n      loadBalancer:\n        servers:\n          - url: \"{addr}\"\n",
                addr = options.control_backend_addr,
            ));
    }

    output.push_str(&format!(
        "    denia-challenge:\n      loadBalancer:\n        servers:\n          - url: \"{addr}\"\n",
        addr = options.control_backend_addr,
    ));

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
    for domain in &route.domains {
        validate_domain(domain)?;
    }
    if route.route_key.is_empty() {
        return Err(TraefikError::EmptyServiceName);
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), TraefikError> {
    if domain.is_empty() || domain.contains('`') || domain.contains('\n') || domain.contains('\r') {
        return Err(TraefikError::InvalidDomain(domain.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec(key: &str, domains: Vec<&str>, port: u16, tls: bool) -> RouteSpec {
        RouteSpec {
            route_key: key.to_string(),
            service_name: key.to_string(),
            domains: domains.into_iter().map(String::from).collect(),
            bridge_port: port,
            tls,
        }
    }

    #[test]
    fn renders_plain_router_without_tls() {
        let routes = vec![make_spec("svc-web", vec!["app"], 9000, false)];
        let yaml =
            render_file_provider_config(&routes, &IngressRenderOptions::test_defaults()).unwrap();
        assert!(!yaml.contains("certResolver"));
        assert!(yaml.contains("entryPoints:\n        - web"));
    }

    #[test]
    fn renders_tls_router_with_resolver_and_redirect() {
        let routes = vec![make_spec("svc-web", vec!["app.example.com"], 9000, true)];
        let opts = IngressRenderOptions::test_defaults();
        let yaml = render_file_provider_config(&routes, &opts).unwrap();
        assert!(yaml.contains("certResolver: le"));
        assert!(yaml.contains("websecure"));
        assert!(yaml.contains("redirectScheme"));
    }

    #[test]
    fn tls_without_resolver_errors() {
        let routes = vec![make_spec("svc-web", vec!["a"], 1, true)];
        let mut opts = IngressRenderOptions::test_defaults();
        opts.acme_resolver = String::new();
        assert_eq!(
            render_file_provider_config(&routes, &opts).unwrap_err(),
            TraefikError::MissingResolver
        );
    }

    #[test]
    fn renders_control_route_with_tls() {
        let routes = vec![];
        let mut opts = IngressRenderOptions::test_defaults();
        opts.control_domain = Some("denia.example.com".into());
        opts.control_tls = true;
        let yaml = render_file_provider_config(&routes, &opts).unwrap();
        assert!(yaml.contains("Host(`denia.example.com`)"));
        assert!(yaml.contains("denia-control"));
        assert!(yaml.contains("websecure"));
    }

    #[test]
    fn render_emits_denia_challenge_router_when_routes_present() {
        let spec = make_spec("svc", vec!["a.example.com"], 9000, false);
        let opts = IngressRenderOptions::test_defaults();
        let out = render_file_provider_config(&[spec], &opts).unwrap();
        assert!(out.contains("denia-challenge:"));
        assert!(out.contains("PathPrefix(`/.well-known/denia-challenge`)"));
        assert!(out.contains("priority: 1000"));
        assert!(out.contains("    denia-challenge:\n      loadBalancer:"));
    }

    #[test]
    fn render_emits_denia_challenge_router_even_with_zero_routes() {
        let opts = IngressRenderOptions::test_defaults();
        let out = render_file_provider_config(&[], &opts).unwrap();
        assert!(out.contains("denia-challenge:"));
    }
}
