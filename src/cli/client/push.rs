//! `denia push`: deploy the current pushed Git branch using `.denia`. Thin API
//! client over `/v1/projects`, `/v1/services`, and `/v1/deployments` — the build
//! runs on the Denia node. See ADR-030.

use super::git;
use super::http::ClientApi;
use super::manifest::DeniaManifest;
use super::profile::{ClientConfig, config_path};
use clap::Args;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Project directory containing the `.denia` manifest.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub async fn run(args: PushArgs) -> anyhow::Result<()> {
    // 1. Read + parse the committed manifest.
    let manifest_path = args.path.join(".denia");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| anyhow::anyhow!("missing .denia at {}: {e}", manifest_path.display()))?;
    let manifest = DeniaManifest::parse(&raw)?;

    // 2. Resolve the active profile (url + token).
    let cfg = ClientConfig::load_from(&config_path()?)?;
    let profile = cfg.active_profile()?;

    // 3. Resolve the Git remote + current branch, and require HEAD to be pushed.
    let remote = &manifest.source.remote;
    let branch = git::current_branch(&args.path)?;
    let repo_url = git::remote_url(&args.path, remote)?;
    git::ensure_head_pushed(&args.path, remote, &branch)?;

    let api = ClientApi::new(&profile.url);
    let token = &profile.token;

    // 4. Resolve the named project.
    let projects = api.list_projects(token).await?;
    let project = projects
        .iter()
        .find(|p| p.name == manifest.project)
        .ok_or_else(|| anyhow::anyhow!("project '{}' not found", manifest.project))?;

    // 5. Create or update the service (upsert keyed on (project_id, name)).
    let existing = api
        .list_services(token)
        .await?
        .into_iter()
        .find(|s| s.project_id == project.id && s.name == manifest.service);

    let mut body = json!({
        "project_id": project.id,
        "name": manifest.service,
        "domains": manifest.ingress.domains,
        "source": {
            "type": "git",
            "repo_url": repo_url,
            "git_ref": branch,
            "dockerfile_path": manifest.source.dockerfile,
            "context_path": manifest.source.context,
            "credential": manifest.source.git_credential_ref,
        },
        "internal_port": manifest.runtime.internal_port,
        "health_check": {
            "path": manifest.health.path,
            "timeout_seconds": manifest.health.timeout_seconds,
        },
        "env": [],
        "tls_enabled": manifest.ingress.tls_enabled,
    });
    if let Some(limits) = &manifest.limits {
        body["resource_limits"] = json!({
            "cpu_millis": limits.cpu_millis,
            "memory_bytes": limits.memory_bytes,
        });
    }
    // Pass the existing id on the update path; omit it on create so the service
    // API mints a server-side UUIDv7.
    if let Some(existing) = &existing {
        body["id"] = json!(existing.id);
    }
    let service = api.put_service(token, &body).await?;

    // 6. Start a Git deployment for the resolved service.
    let deployment = api
        .create_deployment(
            token,
            &json!({
                "source": "git",
                "service_id": service.id,
                "repo_url": repo_url,
                "git_ref": branch,
            }),
        )
        .await?;

    // 7. Report the deployment id and the web console URL.
    println!(
        "Deployment {} started for {} ({}).",
        deployment.id, service.name, deployment.status
    );
    println!("{}", profile.url);
    Ok(())
}
