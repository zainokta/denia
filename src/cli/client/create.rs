//! `denia create`: create a remote service from a `.denia` manifest with an
//! `upload` source, so it is deployed from the local working tree by
//! `denia push` (ADR-039). The shared `create_service_from_manifest` helper is
//! also used by `denia push --create`.

use std::path::PathBuf;

use clap::Args;

use super::http::{ClientApi, ServiceView};
use super::manifest::DeniaManifest;
use super::profile::{ClientConfig, config_path};

#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Directory containing the `.denia` manifest (default: `.`).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub path: PathBuf,

    /// Override the project name from `.denia`.
    #[arg(long)]
    pub project: Option<String>,

    /// Override the service name from `.denia`.
    #[arg(long)]
    pub service: Option<String>,

    /// Override the listen port from the `.denia` `[create]` block.
    #[arg(long)]
    pub port: Option<u16>,

    /// Override the health-check path from the `.denia` `[create]` block.
    #[arg(long, value_name = "PATH")]
    pub health_path: Option<String>,

    /// Profile to use; defaults to the active profile.
    #[arg(long)]
    pub profile: Option<String>,
}

/// Manifest overrides supplied on the command line. Each `Some` value wins over
/// the corresponding `.denia` field.
pub struct CreateOverrides {
    pub project: Option<String>,
    pub service: Option<String>,
    pub port: Option<u16>,
    pub health_path: Option<String>,
}

pub async fn run(args: CreateArgs) -> anyhow::Result<()> {
    // 1. Load client config and pick a profile (mirrors `denia push`).
    let cfg_path = config_path()?;
    let cfg = ClientConfig::load_from(&cfg_path)?;
    let profile = if let Some(ref name) = args.profile {
        cfg.profiles
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("profile '{}' not found", name))?
    } else {
        cfg.active_profile()?
    };
    let url = profile.url.trim_end_matches('/').to_string();
    let token = profile.token.clone();
    let api = ClientApi::new(&url);

    // 2. The manifest is required for `denia create`.
    let manifest_path = args.path.join(".denia");
    let manifest_raw = std::fs::read_to_string(&manifest_path).map_err(|_| {
        anyhow::anyhow!(
            "no .denia manifest at {} — run `denia init` first",
            manifest_path.display()
        )
    })?;
    let manifest = DeniaManifest::parse(&manifest_raw)?;

    let svc = create_service_from_manifest(
        &api,
        &token,
        &manifest,
        CreateOverrides {
            project: args.project,
            service: args.service,
            port: args.port,
            health_path: args.health_path,
        },
    )
    .await?;

    println!("Created service '{}' (id: {})", svc.name, svc.id);
    println!("Deploy it from this directory with `denia push`.");
    Ok(())
}

/// Resolve-or-create the project, then create a service with an `upload` source
/// from the manifest `[create]` defaults. Returns the persisted service.
/// Shared by `denia create` and `denia push --create` (ADR-039).
pub async fn create_service_from_manifest(
    api: &ClientApi,
    token: &str,
    manifest: &DeniaManifest,
    overrides: CreateOverrides,
) -> anyhow::Result<ServiceView> {
    let project = overrides
        .project
        .unwrap_or_else(|| manifest.project.clone());
    let service = overrides
        .service
        .unwrap_or_else(|| manifest.service.clone());

    let create_defaults = manifest.create.as_ref();
    let port = overrides
        .port
        .or_else(|| create_defaults.map(|c| c.port))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "service creation needs a port: add a [create] block with \
                 `port = <port>` to .denia (or pass --port)"
            )
        })?;
    let health_path = overrides
        .health_path
        .or_else(|| create_defaults.and_then(|c| c.health_path.clone()))
        .unwrap_or_else(|| "/".to_string());

    // Resolve the project by name, creating it if it does not exist yet.
    let projects = api.list_projects(token).await?;
    let project_id = if let Some(p) = projects.iter().find(|p| p.name == project) {
        p.id.clone()
    } else {
        api.create_project(token, &project).await?.id
    };

    // `create` is not an upsert: refuse to clobber an existing service so a
    // stray `denia create` cannot silently rewrite a live config.
    let existing = api.list_services(token).await?;
    if existing
        .iter()
        .any(|s| s.name == service && s.project_id == project_id)
    {
        anyhow::bail!(
            "service '{project}/{service}' already exists; deploy to it with `denia push`"
        );
    }

    // Upload source (ADR-039): the build context is supplied per-deploy by
    // `denia push`, so the service stores no source config of its own.
    let service_config_value = serde_json::json!({
        "project_id": project_id,
        "name": service,
        "domains": [],
        "source": { "type": "upload" },
        "internal_port": port,
        "health_check": { "path": health_path, "timeout_seconds": 30 },
        "resource_limits": null,
        "env": [],
        "tls_enabled": false,
        "autoscale": null
    });

    Ok(api.create_service(token, &service_config_value).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_from_manifest_posts_upload_source() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let pid = "01900000-0000-7000-8000-000000000010";
        let _projects = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/projects");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(format!(r#"[{{"id":"{pid}","name":"default"}}]"#));
            })
            .await;
        let _services = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/services");
                then.status(200)
                    .header("content-type", "application/json")
                    .body("[]");
            })
            .await;
        // The posted ServiceConfig must carry an upload source and the
        // manifest's port — assert on the body the server receives.
        let create = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/services").json_body(
                    serde_json::json!({
                        "project_id": pid,
                        "name": "api",
                        "domains": [],
                        "source": { "type": "upload" },
                        "internal_port": 8080,
                        "health_check": { "path": "/", "timeout_seconds": 30 },
                        "resource_limits": null,
                        "env": [],
                        "tls_enabled": false,
                        "autoscale": null
                    }),
                );
                then.status(200)
                    .header("content-type", "application/json")
                    .body(format!(
                        r#"{{"id":"01900000-0000-7000-8000-000000000011","name":"api","project_id":"{pid}"}}"#
                    ));
            })
            .await;

        let api = ClientApi::new(&server.base_url());
        let manifest =
            DeniaManifest::parse("project=\"default\"\nservice=\"api\"\n[create]\nport=8080\n")
                .unwrap();
        let svc = create_service_from_manifest(
            &api,
            "tok",
            &manifest,
            CreateOverrides {
                project: None,
                service: None,
                port: None,
                health_path: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(svc.name, "api");
        create.assert_async().await;
    }

    #[tokio::test]
    async fn create_from_manifest_rejects_existing_service() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let pid = "01900000-0000-7000-8000-000000000010";
        let _projects = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/projects");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(format!(r#"[{{"id":"{pid}","name":"default"}}]"#));
            })
            .await;
        let _services = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/services");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(format!(
                        r#"[{{"id":"s1","name":"api","project_id":"{pid}"}}]"#
                    ));
            })
            .await;

        let api = ClientApi::new(&server.base_url());
        let manifest =
            DeniaManifest::parse("project=\"default\"\nservice=\"api\"\n[create]\nport=8080\n")
                .unwrap();
        let err = create_service_from_manifest(
            &api,
            "tok",
            &manifest,
            CreateOverrides {
                project: None,
                service: None,
                port: None,
                health_path: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }
}
