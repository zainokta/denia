//! `denia profile`: inspect and switch stored client profiles. See ADR-030.

use super::profile::{ClientConfig, config_path};
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub command: ProfileCommand,
}

#[derive(Subcommand, Debug)]
pub enum ProfileCommand {
    /// List configured profiles (active marked with `*`).
    List,
    /// Print the active profile name and URL.
    Show,
    /// Set the active profile.
    Use { name: String },
}

pub fn run(args: ProfileArgs) -> anyhow::Result<()> {
    let path = config_path()?;
    match args.command {
        ProfileCommand::List => {
            let cfg = ClientConfig::load_from(&path)?;
            for profile in &cfg.profiles {
                let marker = if profile.name == cfg.active { "*" } else { " " };
                println!("{marker} {} ({})", profile.name, profile.url);
            }
            Ok(())
        }
        ProfileCommand::Show => {
            let cfg = ClientConfig::load_from(&path)?;
            let profile = cfg.active_profile()?;
            println!("{} {}", profile.name, profile.url);
            Ok(())
        }
        ProfileCommand::Use { name } => {
            let mut cfg = ClientConfig::load_from(&path)?;
            if !cfg.profiles.iter().any(|p| p.name == name) {
                anyhow::bail!("profile not found: {name}");
            }
            cfg.active = name;
            cfg.save_to(&path)?;
            Ok(())
        }
    }
}
