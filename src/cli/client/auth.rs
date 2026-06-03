//! `denia auth`: interactive login → session token → mint long-lived API token
//! → save to client profile. See ADR-034.

use std::io::{BufRead, Write};

use clap::Args;

use super::http::ClientApi;
use super::profile::{ClientConfig, Profile, config_path};

#[derive(Args, Debug)]
pub struct AuthArgs {
    /// Remote Denia URL (e.g. https://denia.example.com). Prompted if omitted.
    #[arg(long)]
    pub url: Option<String>,
    /// Username. Prompted if omitted.
    #[arg(long)]
    pub username: Option<String>,
    /// Profile name to save under. Defaults to the URL host.
    #[arg(long)]
    pub profile: Option<String>,
    /// Name for the minted API token.
    #[arg(long, default_value = "denia-cli")]
    pub token_name: String,
    /// Read the password from stdin (one line) instead of prompting (for scripts/tests).
    #[arg(long)]
    pub password_stdin: bool,
}

/// Prompt the user for a line of text on stdout/stdin.
fn prompt(question: &str) -> anyhow::Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Read a password without echoing characters, using crossterm raw mode.
/// Reads until Enter is pressed. A newline is printed after entry.
fn read_password(question: &str) -> anyhow::Result<String> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEvent},
        terminal,
    };

    print!("{question}");
    std::io::stdout().flush()?;

    terminal::enable_raw_mode()?;
    let mut password = String::new();
    loop {
        match event::read() {
            Ok(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Enter => break,
                KeyCode::Char(c) => password.push(c),
                KeyCode::Backspace => {
                    password.pop();
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => {
                terminal::disable_raw_mode()?;
                return Err(e.into());
            }
        }
    }
    terminal::disable_raw_mode()?;
    println!();
    Ok(password)
}

pub async fn run(args: AuthArgs) -> anyhow::Result<()> {
    // 1. Resolve URL.
    let url = match args.url {
        Some(u) => u,
        None => prompt("Denia URL: ")?,
    };
    let url = url.trim_end_matches('/').to_string();

    // 2. Probe reachability via GET /healthz.
    let api = ClientApi::new(&url);
    {
        let probe = reqwest::Client::new()
            .get(format!("{url}/healthz"))
            .send()
            .await;
        match probe {
            Err(e) => anyhow::bail!("cannot reach Denia at {url}: {e}"),
            Ok(resp) if !resp.status().is_success() => {
                anyhow::bail!(
                    "cannot reach Denia at {url}: /healthz returned {}",
                    resp.status()
                )
            }
            Ok(_) => {}
        }
    }

    // 3. Resolve username.
    let username = match args.username {
        Some(u) => u,
        None => prompt("Username: ")?,
    };

    // 4. Read password (no echo).
    let password = if args.password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim_end_matches(['\n', '\r']).to_string()
    } else {
        read_password("Password: ")?
    };

    // 5. Login — session token stays in memory only.
    let session = api.login(&username, &password).await?;

    // 6. Mint a long-lived API token.
    let minted = api
        .create_api_token(&session.token, &args.token_name)
        .await?;

    // 7. Verify the minted token works.
    api.me(&minted.token).await?;

    // 8. Save to profile.
    let profile_name = args
        .profile
        .clone()
        .or_else(|| {
            reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
        })
        .unwrap_or_else(|| "default".to_string());

    let cfg_path = config_path()?;
    let mut cfg = ClientConfig::load_from(&cfg_path).unwrap_or_default();
    cfg.upsert_profile(
        &profile_name,
        Profile {
            url: url.clone(),
            token: minted.token,
        },
    );
    cfg.set_active(&profile_name);
    cfg.save_to(&cfg_path)?;

    // 9. Confirm — never print the token or password.
    println!(
        "Authenticated as {username}; saved profile '{profile_name}' to {}.",
        cfg_path.display()
    );

    Ok(())
}
