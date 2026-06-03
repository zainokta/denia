//! `denia auth`: authenticate this machine to a Denia control plane and store a
//! named API token profile. See ADR-030. (Implemented in a later task.)

use clap::Args;

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

pub async fn run(_args: AuthArgs) -> anyhow::Result<()> {
    anyhow::bail!("denia auth is not yet implemented")
}
