//! Subcommand surface for the denia binary. See ADR-025 + spec
//! 2026-05-28-denia-binary-subcommands-design.md, and ADR-030 for the
//! client/server command split.
//!
//! Client commands (`auth`, `push`, `profile`) compile on every platform. The
//! `denia server ...` group and its modules are gated behind the `server`
//! feature so a client-only build pulls no Linux-runtime/ingress code.

pub mod client;

#[cfg(feature = "server")]
pub mod common;
#[cfg(feature = "server")]
pub mod doctor;
#[cfg(feature = "server")]
pub mod rotate_token;
#[cfg(feature = "server")]
pub mod setup;
#[cfg(feature = "server")]
pub mod status;
#[cfg(feature = "server")]
pub mod uninstall;
#[cfg(feature = "server")]
pub mod update;

use clap::Parser;
#[cfg(feature = "server")]
use clap::Subcommand;
#[cfg(feature = "server")]
use setup::SetupArgs;
#[cfg(feature = "server")]
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

// The command enum is defined twice so clap never has to reason about a
// `#[cfg]`-gated variant: server builds expose the `server` group, client builds
// do not.
#[cfg(feature = "server")]
#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Authenticate this machine to a Denia control plane.
    Auth(client::auth::AuthArgs),
    /// Deploy the current pushed Git branch using `.denia`.
    Push(client::push::PushArgs),
    /// Manage client profiles.
    Profile(client::profile_command::ProfileArgs),
    /// Linux host/server commands.
    #[command(subcommand)]
    Server(ServerCommands),
}

#[cfg(not(feature = "server"))]
#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Authenticate this machine to a Denia control plane.
    Auth(client::auth::AuthArgs),
    /// Deploy the current pushed Git branch using `.denia`.
    Push(client::push::PushArgs),
    /// Manage client profiles.
    Profile(client::profile_command::ProfileArgs),
}

#[cfg(feature = "server")]
#[derive(Subcommand, Debug)]
pub enum ServerCommands {
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
    /// Run the control plane + ingress daemon (foreground).
    Run,
}

/// Entry point called from main.rs (server build).
///
/// For migration, invoking the binary with no subcommand still starts the
/// daemon, but `denia server run` is the documented entrypoint (the systemd
/// unit uses it).
#[cfg(feature = "server")]
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    apply_config_override(cli.config.as_ref());
    match cli.command {
        Some(Commands::Auth(args)) => block_on(crate::cli::client::auth::run(args)),
        Some(Commands::Push(args)) => block_on(crate::cli::client::push::run(args)),
        Some(Commands::Profile(args)) => crate::cli::client::profile_command::run(args),
        Some(Commands::Server(command)) => dispatch_server(command),
        None => run_daemon(),
    }
}

/// Entry point called from main.rs (client-only build).
#[cfg(not(feature = "server"))]
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Commands::Auth(args)) => block_on(crate::cli::client::auth::run(args)),
        Some(Commands::Push(args)) => block_on(crate::cli::client::push::run(args)),
        Some(Commands::Profile(args)) => crate::cli::client::profile_command::run(args),
        None => anyhow::bail!("no command given; run `denia --help`"),
    }
}

#[cfg(feature = "server")]
fn apply_config_override(config: Option<&std::path::PathBuf>) {
    if let Some(path) = config {
        // SAFETY: single-threaded here; dispatch runs before the daemon's
        // tokio runtime (or any other threads) is created.
        unsafe {
            std::env::set_var("DENIA_CONFIG_FILE", path);
        }
    }
}

#[cfg(feature = "server")]
fn dispatch_server(command: ServerCommands) -> anyhow::Result<()> {
    match command {
        ServerCommands::Setup(args) => crate::cli::setup::run(args),
        ServerCommands::Uninstall(args) => crate::cli::uninstall::run(args),
        ServerCommands::Status => crate::cli::status::run(),
        ServerCommands::Doctor => crate::cli::doctor::run(),
        ServerCommands::RotateToken => crate::cli::rotate_token::run(),
        ServerCommands::Update(args) => crate::cli::update::run(args),
        ServerCommands::Run => run_daemon(),
    }
}

/// Run the async daemon on a freshly built tokio runtime so non-daemon
/// subcommands never pay for one.
#[cfg(feature = "server")]
fn run_daemon() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::daemon::run())
}

/// Run an async client command on a freshly built tokio runtime.
fn block_on<F>(fut: F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fut)
}
