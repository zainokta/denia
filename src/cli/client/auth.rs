//! `denia auth`: authenticate this machine to a Denia control plane and store a
//! named API token profile. See ADR-030.

use super::http::ClientApi;
use super::profile::{ClientConfig, ClientProfile, config_path};
use clap::Args;
use std::io::{BufRead, Write};

#[derive(Args, Debug)]
pub struct AuthArgs {
    /// Denia control plane URL (e.g. https://denia.example.com).
    #[arg(long)]
    pub url: String,
    /// Username to authenticate as.
    #[arg(long)]
    pub username: String,
    /// Read the password from stdin instead of prompting interactively.
    #[arg(long)]
    pub password_stdin: bool,
    /// Profile name to store the credentials under.
    #[arg(long, default_value = "default")]
    pub profile: String,
}

pub async fn run(args: AuthArgs) -> anyhow::Result<()> {
    let password = read_password(args.password_stdin)?;

    let api = ClientApi::new(&args.url);
    let session = api.login(&args.username, &password).await?;
    let created = api.create_api_token(&session.token, &token_name()).await?;
    // Confirm the freshly minted API token authenticates before persisting it.
    api.verify_me(&created.token).await?;

    let path = config_path()?;
    let mut cfg = ClientConfig::load_from(&path).unwrap_or_else(|_| ClientConfig {
        active: args.profile.clone(),
        profiles: Vec::new(),
    });
    let profile = ClientProfile {
        name: args.profile.clone(),
        url: args.url.trim_end_matches('/').to_string(),
        token: created.token,
    };
    cfg.profiles.retain(|p| p.name != profile.name);
    cfg.profiles.push(profile);
    cfg.active = args.profile.clone();
    cfg.save_to(&path)?;

    println!(
        "Authenticated. Profile '{}' saved for {}.",
        args.profile, args.url
    );
    Ok(())
}

/// Read the password from stdin (one line). With `--password-stdin` the caller
/// pipes it in; otherwise prompt on stderr and read a line. The prompt is not
/// hidden, so prefer `--password-stdin` in scripts.
fn read_password(from_stdin: bool) -> anyhow::Result<String> {
    if !from_stdin {
        eprint!("Password: ");
        std::io::stderr().flush()?;
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let password = line.trim_end_matches(['\n', '\r']).to_string();
    if password.is_empty() {
        anyhow::bail!("empty password");
    }
    Ok(password)
}

/// Name the created API token after the local host so operators can tell
/// machines apart in the token list.
fn token_name() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "client".to_string());
    format!("denia-cli@{host}")
}
