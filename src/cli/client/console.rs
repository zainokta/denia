//! `denia console`: open an interactive `/bin/sh` inside a running service
//! replica from any workstation, over the ticket + websocket protocol. See
//! ADR-033.

use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::http::{ClientApi, ServiceView};
use super::manifest::DeniaManifest;
use super::profile::{ClientConfig, config_path};

#[derive(Args, Debug)]
pub struct ConsoleArgs {
    /// Service id or service name. Omit to read service/project from `.denia`.
    pub service: Option<String>,
    /// Project name for service-name resolution.
    #[arg(long)]
    pub project: Option<String>,
    /// Replica index to attach to.
    #[arg(long)]
    pub replica: Option<u32>,
    /// Project directory containing `.denia` when SERVICE is omitted.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub async fn run(args: ConsoleArgs) -> anyhow::Result<()> {
    let cfg = ClientConfig::load_from(&config_path()?)?;
    let profile = cfg.active_profile()?;
    let api = ClientApi::new(&profile.url);
    let token = &profile.token;

    let manifest = read_manifest_if_present(&args.path)?;
    let project_name = args
        .project
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.project.clone()));
    let service_name = args
        .service
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.service.clone()))
        .ok_or_else(|| anyhow::anyhow!("service is required when .denia is not present"))?;

    let service = resolve_service(&api, token, project_name.as_deref(), &service_name).await?;
    let replicas = api.list_console_replicas(token, &service.id).await?;
    if replicas.is_empty() {
        anyhow::bail!("service '{}' has no running replicas", service.name);
    }
    let replica_index = match args.replica {
        Some(index) => index,
        None if replicas.len() == 1 => replicas[0].replica_index,
        None => {
            eprintln!("service '{}' has multiple running replicas:", service.name);
            for replica in &replicas {
                eprintln!(
                    "  replica={} deployment={} state={}",
                    replica.replica_index, replica.deployment_id, replica.state
                );
            }
            anyhow::bail!("choose a replica with --replica <INDEX>");
        }
    };

    let (cols, rows) = terminal_size();
    let ticket = api
        .create_console_ticket(token, &service.id, replica_index, cols, rows)
        .await?;
    let ws_url = api.websocket_url(&ticket.ws_path)?;
    run_terminal(ws_url).await
}

fn read_manifest_if_present(path: &std::path::Path) -> anyhow::Result<Option<DeniaManifest>> {
    let manifest_path = path.join(".denia");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    Ok(Some(DeniaManifest::parse(&raw)?))
}

async fn resolve_service(
    api: &ClientApi,
    token: &str,
    project_name: Option<&str>,
    service: &str,
) -> anyhow::Result<ServiceView> {
    let services = api.list_services(token).await?;
    if uuid::Uuid::parse_str(service).is_ok() {
        return services
            .into_iter()
            .find(|s| s.id == service)
            .ok_or_else(|| anyhow::anyhow!("service id '{}' not found", service));
    }
    let project_id = match project_name {
        Some(name) => {
            let projects = api.list_projects(token).await?;
            Some(
                projects
                    .into_iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| anyhow::anyhow!("project '{}' not found", name))?
                    .id,
            )
        }
        None => None,
    };
    let matches = services
        .into_iter()
        .filter(|s| s.name == service)
        .filter(|s| project_id.as_ref().is_none_or(|id| &s.project_id == id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => anyhow::bail!("service '{}' not found", service),
        _ => anyhow::bail!("service name '{}' is ambiguous; pass --project", service),
    }
}

fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((120, 32))
}

async fn run_terminal(ws_url: String) -> anyhow::Result<()> {
    let (stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let (mut write, mut read) = stream.split();
    let _raw = RawModeGuard::enter()?;

    // Stream stdin over a channel so input is forwarded as it is typed, rather
    // than buffered until EOF.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0_u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    loop {
        tokio::select! {
            chunk = rx.recv() => {
                match chunk {
                    Some(bytes) => write.send(Message::Binary(bytes.into())).await?,
                    None => {
                        let _ = write.close().await;
                        break;
                    }
                }
            }
            message = read.next() => {
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        let mut stdout = std::io::stdout();
                        stdout.write_all(&bytes)?;
                        stdout.flush()?;
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text.as_str().contains("\"type\":\"exit\"") {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                }
            }
        }
    }
    Ok(())
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> anyhow::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
