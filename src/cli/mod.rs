//! Subcommand surface for the denia binary. See ADR-025 + ADR-036.
//!
//! Client subcommands (`auth`, `push`, `console`) are portable and compile on
//! every target. Host/server subcommands (`setup`, `uninstall`, `status`,
//! `doctor`, `rotate-token`, `update`) and the daemon are Linux-only and are
//! gated out on macOS/Windows, where `denia` is a client-only binary.

pub mod client;

#[cfg(target_os = "linux")]
pub mod common;
#[cfg(target_os = "linux")]
pub mod doctor;
#[cfg(target_os = "linux")]
pub mod rotate_token;
#[cfg(target_os = "linux")]
pub mod setup;
#[cfg(target_os = "linux")]
pub mod status;
#[cfg(target_os = "linux")]
pub mod uninstall;
#[cfg(target_os = "linux")]
pub mod update;

use clap::{Parser, Subcommand};
#[cfg(target_os = "linux")]
use setup::SetupArgs;
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    Setup(SetupArgs),
    /// Tear down the service. With --purge: also wipe data + user + config.
    #[cfg(target_os = "linux")]
    Uninstall(UninstallArgs),
    /// Print live service state.
    #[cfg(target_os = "linux")]
    Status,
    /// Diagnose host + denia install.
    #[cfg(target_os = "linux")]
    Doctor,
    /// Regenerate the admin token and restart the service.
    #[cfg(target_os = "linux")]
    RotateToken,
    /// Self-update from the latest signed GitHub release and restart.
    #[cfg(target_os = "linux")]
    Update(update::UpdateArgs),
    /// Open an interactive shell inside a running service replica.
    Console(client::console::ConsoleArgs),
    /// Authenticate to a remote Denia instance and save credentials locally.
    Auth(client::auth::AuthArgs),
    /// Pack the local working tree and deploy it to a remote service.
    Push(client::push::PushArgs),
}

/// Entry point called from main.rs. The daemon is run when no subcommand is
/// given on Linux; on other platforms only the client subcommands exist.
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    if let Some(path) = cli.config.as_ref() {
        // SAFETY: single-threaded here; dispatch runs before the daemon's
        // tokio runtime (or any other threads) is created.
        unsafe {
            std::env::set_var("DENIA_CONFIG_FILE", path);
        }
    }
    match cli.command {
        #[cfg(target_os = "linux")]
        Some(Commands::Setup(args)) => crate::cli::setup::run(args),
        #[cfg(target_os = "linux")]
        Some(Commands::Uninstall(args)) => crate::cli::uninstall::run(args),
        #[cfg(target_os = "linux")]
        Some(Commands::Status) => crate::cli::status::run(),
        #[cfg(target_os = "linux")]
        Some(Commands::Doctor) => crate::cli::doctor::run(),
        #[cfg(target_os = "linux")]
        Some(Commands::RotateToken) => crate::cli::rotate_token::run(),
        #[cfg(target_os = "linux")]
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
        Some(Commands::Push(args)) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::cli::client::push::run(args))
        }
        #[cfg(target_os = "linux")]
        None => {
            // Daemon is async; build a runtime here so non-daemon subcommands
            // never pay for one.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::daemon::run())
        }
        #[cfg(not(target_os = "linux"))]
        None => anyhow::bail!(
            "no subcommand given; the Denia daemon runs on Linux only. \
             Use a client subcommand: auth, push, or console."
        ),
    }
}
