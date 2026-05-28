//! Subcommand surface for the denia binary. See ADR-025 + spec
//! 2026-05-28-denia-binary-subcommands-design.md.

pub mod common;
pub mod doctor;
pub mod rotate_token;
pub mod setup;
pub mod status;
pub mod uninstall;

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
}

/// Entry point called from main.rs. Subcommand variants return placeholder
/// errors until subsequent tasks implement them; the daemon is run when no
/// subcommand is given.
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Commands::Setup(args)) => crate::cli::setup::run(args),
        Some(Commands::Uninstall(args)) => crate::cli::uninstall::run(args),
        Some(Commands::Status) => crate::cli::status::run(),
        Some(Commands::Doctor) => crate::cli::doctor::run(),
        Some(Commands::RotateToken) => crate::cli::rotate_token::run(),
        None => {
            // Daemon is async; build a runtime here so non-daemon subcommands
            // never pay for one.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::daemon::run())
        }
    }
}
