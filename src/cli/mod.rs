//! Subcommand surface for the denia binary. See ADR-024 + spec
//! 2026-05-28-denia-binary-subcommands-design.md.

pub mod common;

use clap::{Parser, Subcommand};

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
    Setup,
    /// Tear down the service. With --purge: also wipe data + user + config.
    Uninstall {
        /// Also wipe /var/lib/denia, ~/.config/denia, and the denia system user.
        #[arg(long)]
        purge: bool,
        /// Print the plan without executing it.
        #[arg(long)]
        dry_run: bool,
    },
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
        Some(Commands::Setup) => Err(anyhow::anyhow!("setup not yet implemented")),
        Some(Commands::Uninstall { .. }) => Err(anyhow::anyhow!("uninstall not yet implemented")),
        Some(Commands::Status) => Err(anyhow::anyhow!("status not yet implemented")),
        Some(Commands::Doctor) => Err(anyhow::anyhow!("doctor not yet implemented")),
        Some(Commands::RotateToken) => Err(anyhow::anyhow!("rotate-token not yet implemented")),
        None => {
            // Daemon is async; build a runtime here so non-daemon subcommands
            // never pay for one.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::daemon::run())
        }
    }
}
