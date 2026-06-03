//! `denia push`: pack the local working tree and deploy it to a remote service.
//! See ADR-034.

use std::path::PathBuf;

use clap::Args;

use super::http::ClientApi;
use super::manifest::DeniaManifest;
use super::pack::{PackLimits, pack_context};
use super::profile::{ClientConfig, config_path};

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Create the service if it does not exist yet (requires `[create]` block in
    /// `.denia` and a configured `control_domain` on the node).
    #[arg(long)]
    pub create: bool,

    /// Override the project name from `.denia`.
    #[arg(long)]
    pub project: Option<String>,

    /// Override the service name from `.denia`.
    #[arg(long)]
    pub service: Option<String>,

    /// Override the Dockerfile path from `.denia`.
    #[arg(long)]
    pub dockerfile: Option<String>,

    /// Override the build context path from `.denia`.
    #[arg(long, value_name = "DIR")]
    pub context: Option<String>,

    /// Directory containing the `.denia` manifest (default: `.`).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub path: PathBuf,

    /// Profile to use; defaults to the active profile.
    #[arg(long)]
    pub profile: Option<String>,

    /// Print the deployment ID and return without following logs or polling
    /// for a terminal status.
    #[arg(long)]
    pub no_follow: bool,
}

pub async fn run(args: PushArgs) -> anyhow::Result<()> {
    // 1. Load client config and pick a profile.
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

    // 2. Read the manifest and resolve all parameters.
    let manifest_path = args.path.join(".denia");
    let manifest_raw = std::fs::read_to_string(&manifest_path).ok();
    let manifest: Option<DeniaManifest> = match manifest_raw.as_deref() {
        Some(raw) => Some(DeniaManifest::parse(raw)?),
        None => None,
    };

    let project = args
        .project
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.project.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no project specified: pass --project <name> or create a .denia manifest at {}",
                manifest_path.display()
            )
        })?;
    let service = args
        .service
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.service.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no service specified: pass --service <name> or create a .denia manifest at {}",
                manifest_path.display()
            )
        })?;

    let dockerfile_rel = args.dockerfile.clone().unwrap_or_else(|| {
        manifest
            .as_ref()
            .map(|m| m.dockerfile().to_string())
            .unwrap_or_else(|| "Dockerfile".to_string())
    });
    let context_rel = args.context.clone().unwrap_or_else(|| {
        manifest
            .as_ref()
            .map(|m| m.context().to_string())
            .unwrap_or_else(|| ".".to_string())
    });

    // 3. Resolve the target service.
    let projects = api.list_projects(&token).await?;
    let services = api.list_services(&token).await?;

    // Build a map of project_id → name for fast lookup.
    let project_id_to_name: std::collections::HashMap<String, String> = projects
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();

    let found_svc = services.iter().find(|s| {
        s.name == service
            && project_id_to_name
                .get(&s.project_id)
                .map(|n| n == &project)
                .unwrap_or(false)
    });

    let service_id: String = if let Some(svc) = found_svc {
        svc.id.clone()
    } else if !args.create {
        anyhow::bail!(
            "service '{project}/{service}' not found; create it in the web console or pass --create"
        );
    } else {
        // --create path.
        let node = api.node_info(&token).await?;
        let domain = node
            .control_domain
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--create requires a control domain configured on the node \
                 (hosted registry unavailable); create the service in the web console instead"
                )
            })?;

        // Resolve or create project.
        let project_id = if let Some(p) = projects.iter().find(|p| p.name == project) {
            p.id.clone()
        } else {
            api.create_project(&token, &project).await?.id
        };

        // Determine port and health_path from the manifest [create] block.
        let create_defaults = manifest
            .as_ref()
            .and_then(|m| m.create.as_ref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service not found and --create was given, but the .denia manifest has no \
                     [create] block; add `[create]\\nport = <port>` to .denia and try again"
                )
            })?;
        let port = create_defaults.port;
        let health_path = create_defaults
            .health_path
            .as_deref()
            .unwrap_or("/")
            .to_string();

        // Build the hosted-registry image ref.
        let image_ref = format!("{}/{}/{}:latest", domain, project, service);

        // Build a full ServiceConfig JSON.
        // ServiceSource serde: `#[serde(tag = "type", rename_all = "snake_case")]`
        // → {"type":"external_image","image":"...","credential":null,...}
        // HealthCheck: {"path":"/","timeout_seconds":30}
        let service_config_value = serde_json::json!({
            "project_id": project_id,
            "name": service,
            "domains": [],
            "source": {
                "type": "external_image",
                "image": image_ref,
                "credential": null,
                "registry_id": null,
                "image_ref": null
            },
            "internal_port": port,
            "health_check": {
                "path": health_path,
                "timeout_seconds": 30
            },
            "resource_limits": null,
            "env": [],
            "tls_enabled": false,
            "autoscale": null
        });

        let svc = api.create_service(&token, &service_config_value).await?;
        println!("Created service '{project}/{service}' (id: {})", svc.id);
        svc.id
    };

    // 4. Verify the Dockerfile exists on disk.
    let context_root = args.path.join(&context_rel);
    let dockerfile_abs = context_root.join(&dockerfile_rel);
    if !dockerfile_abs.is_file() {
        anyhow::bail!("no Dockerfile at {} (required)", dockerfile_abs.display());
    }

    // 5. Pack the context.
    let tmp = std::env::temp_dir().join(format!("denia-push-{}.tar.zst", uuid_v7_hex()));
    pack_context(
        &context_root,
        &dockerfile_rel,
        &tmp,
        &PackLimits {
            max_files: 50_000,
            max_bytes: 512 * 1024 * 1024,
        },
    )
    .map_err(|e| anyhow::anyhow!("context pack failed: {e}"))?;

    // 6. Upload the packed context.
    let bytes = std::fs::read(&tmp)?;
    // Clean up temp file regardless of outcome.
    let _ = std::fs::remove_file(&tmp);
    println!("Uploading context ({} bytes)…", bytes.len());
    let up = api.upload_context(&token, &service_id, bytes).await?;

    // 7. Create the deployment.
    let deploy_body = serde_json::json!({
        "source": "upload",
        "service_id": service_id,
        "upload_id": up.upload_id,
        "dockerfile_path": dockerfile_rel,
        "context_path": context_rel
    });
    let dep = api.create_deployment(&token, &deploy_body).await?;
    println!("Deployment {} created", dep.id);

    // 8. Follow logs and poll status unless --no-follow.
    if args.no_follow {
        return Ok(());
    }

    // Best-effort log stream.
    api.stream_deployment_logs(&token, &dep.id).await.ok();

    // Poll for terminal status.
    const MAX_POLLS: u32 = 120; // up to ~10 minutes at 5 s interval
    for _ in 0..MAX_POLLS {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match api.get_deployment(&token, &dep.id).await {
            Ok(view) => match view.status.as_str() {
                "Healthy" => {
                    println!("Deployment {} is Healthy", dep.id);
                    return Ok(());
                }
                "Failed" => {
                    eprintln!("Deployment {} failed", dep.id);
                    std::process::exit(1);
                }
                other => {
                    println!("  status: {other}");
                }
            },
            Err(e) => {
                eprintln!("poll error (will retry): {e}");
            }
        }
    }

    anyhow::bail!(
        "deployment {} did not reach a terminal state within the polling window",
        dep.id
    )
}

/// Generate a short hex string from a UUIDv7 for temp file naming.
fn uuid_v7_hex() -> String {
    let id = uuid::Uuid::now_v7();
    hex::encode(id.as_bytes())
}
