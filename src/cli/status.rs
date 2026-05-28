//! `denia status`: print live service state. No privilege required.

use std::time::Duration;

use crate::config::{config_file_path, FileConfig};

use super::common::systemd;

pub fn run() -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");

    let config_path = config_file_path();
    let file_cfg: Option<FileConfig> = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|raw| toml::from_str::<FileConfig>(&raw).ok())
    } else {
        None
    };
    let bind_addr = file_cfg
        .as_ref()
        .and_then(|c| c.bind_addr.clone())
        .unwrap_or_else(|| "127.0.0.1:7180".to_string());

    let service_active = systemd::is_active("denia.service");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let healthz = rt.block_on(probe_healthz(&bind_addr));

    println!("denia {version}");
    println!(
        "service:    {}",
        if service_active { "active" } else { "inactive" }
    );
    println!("config:     {}", config_path.display());
    println!("bind_addr:  {bind_addr}");
    match &healthz {
        Ok(code) => println!("healthz:    {code} {}", code.canonical_reason().unwrap_or("")),
        Err(e) => println!("healthz:    unreachable ({e})"),
    }

    let ok = service_active && matches!(healthz, Ok(ref c) if c.is_success());
    if ok {
        Ok(())
    } else {
        // Non-zero exit; the actual diagnostic was already printed.
        std::process::exit(1);
    }
}

async fn probe_healthz(bind_addr: &str) -> Result<reqwest::StatusCode, reqwest::Error> {
    let url = format!("http://{bind_addr}/healthz");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let resp = client.get(&url).send().await?;
    Ok(resp.status())
}
