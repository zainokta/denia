//! `denia push`: deploy the current pushed Git branch using `.denia`. See
//! ADR-030. (Implemented in a later task.)

use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Project directory containing the `.denia` manifest.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub async fn run(_args: PushArgs) -> anyhow::Result<()> {
    anyhow::bail!("denia push is not yet implemented")
}
