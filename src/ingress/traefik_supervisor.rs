use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TraefikStaticOptions {
    pub http_port: u16,
    pub https_port: u16,
    pub dynamic_dir: PathBuf,
    pub acme_email: String,
    pub acme_storage: PathBuf,
    pub resolver: String,
}

pub fn render_static_config(opts: &TraefikStaticOptions) -> String {
    format!(
        "entryPoints:\n\
\x20 web:\n\
\x20   address: \":{http}\"\n\
\x20 websecure:\n\
\x20   address: \":{https}\"\n\
providers:\n\
\x20 file:\n\
\x20   directory: \"{dir}\"\n\
\x20   watch: true\n\
certificatesResolvers:\n\
\x20 {resolver}:\n\
\x20   acme:\n\
\x20     email: \"{email}\"\n\
\x20     storage: \"{storage}\"\n\
\x20     httpChallenge:\n\
\x20       entryPoint: web\n",
        http = opts.http_port,
        https = opts.https_port,
        dir = opts.dynamic_dir.display(),
        resolver = opts.resolver,
        email = opts.acme_email,
        storage = opts.acme_storage.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> TraefikStaticOptions {
        TraefikStaticOptions {
            http_port: 80,
            https_port: 443,
            dynamic_dir: PathBuf::from("/var/lib/denia/traefik/dynamic"),
            acme_email: "ops@example.com".into(),
            acme_storage: PathBuf::from("/var/lib/denia/traefik/acme.json"),
            resolver: "le".into(),
        }
    }

    #[test]
    fn renders_entrypoints_and_file_provider() {
        let y = render_static_config(&opts());
        assert!(y.contains("address: \":80\""));
        assert!(y.contains("address: \":443\""));
        assert!(y.contains("directory: \"/var/lib/denia/traefik/dynamic\""));
        assert!(y.contains("watch: true"));
    }

    #[test]
    fn renders_acme_resolver() {
        let y = render_static_config(&opts());
        assert!(y.contains("le:"));
        assert!(y.contains("email: \"ops@example.com\""));
        assert!(y.contains("storage: \"/var/lib/denia/traefik/acme.json\""));
        assert!(y.contains("entryPoint: web"));
    }
}
