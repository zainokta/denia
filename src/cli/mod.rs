//! Subcommand surface for the denia binary. See ADR-025 + spec
//! 2026-05-28-denia-binary-subcommands-design.md.

pub mod client;
pub mod common;
pub mod doctor;
pub mod rotate_token;
pub mod setup;
pub mod status;
pub mod uninstall;
pub mod update;

use clap::{Parser, Subcommand};
use setup::SetupArgs;
use uninstall::UninstallArgs;

#[derive(Parser, Debug)]
#[command(
    name = "denia",
    version = env!("CARGO_PKG_VERSION"),
    about = "Denia single-node PaaS",
    long_about = None,
)]
pub struct Cli {
    /// Path to the TOML config file. Overrides the default search (operator
    /// home via `$SUDO_USER`, then `XDG_CONFIG_HOME`, then `$HOME/.config`).
    /// Same effect as setting `DENIA_CONFIG_FILE`.
    #[arg(short = 'c', long = "config", value_name = "PATH", global = true)]
    pub config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Provision the host: user, dirs, keys, config, systemd unit, start.
    Setup(SetupArgs),
    /// Tear down the service. With --purge: also wipe data + user + config.
    Uninstall(UninstallArgs),
    /// Print live service state.
    Status,
    /// Diagnose host + denia install.
    Doctor,
    /// Regenerate the admin token and restart the service.
    RotateToken,
    /// Self-update from the latest signed GitHub release and restart.
    Update(update::UpdateArgs),
    /// Open an interactive shell inside a running service replica.
    Console(client::console::ConsoleArgs),
    /// Authenticate to a remote Denia instance and save credentials locally.
    Auth(client::auth::AuthArgs),
}

/// Entry point called from main.rs. Subcommand variants return placeholder
/// errors until subsequent tasks implement them; the daemon is run when no
/// subcommand is given.
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    if let Some(path) = cli.config.as_ref() {
        // SAFETY: single-threaded here; dispatch runs before the daemon's
        // tokio runtime (or any other threads) is created.
        unsafe {
            std::env::set_var("DENIA_CONFIG_FILE", path);
        }
    }
    match cli.command {
        Some(Commands::Setup(args)) => crate::cli::setup::run(args),
        Some(Commands::Uninstall(args)) => crate::cli::uninstall::run(args),
        Some(Commands::Status) => crate::cli::status::run(),
        Some(Commands::Doctor) => crate::cli::doctor::run(),
        Some(Commands::RotateToken) => crate::cli::rotate_token::run(),
        Some(Commands::Update(args)) => crate::cli::update::run(args),
        Some(Commands::Console(args)) => {
            // Client console is async (websocket + PTY bridge); build a runtime
            // only for this path, like the daemon arm below.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::cli::client::console::run(args))
        }
        Some(Commands::Auth(args)) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::cli::client::auth::run(args))
        }
        None => {
            // Daemon is async; build a runtime here so non-daemon subcommands
            // never pay for one.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::daemon::run())
        }
    }
}
