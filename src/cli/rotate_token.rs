//! `denia rotate-token`: regenerate the admin bearer + restart the service.

use std::time::Duration;

use super::common::{io, paths::InstallContext, privilege, secrets, systemd};

pub fn run() -> anyhow::Result<()> {
    privilege::require_root()?;
    let ctx = privilege::detect_install_user()?;

    let token = secrets::generate_admin_token();
    write_token_file(&ctx, &token)?;
    println!("==> wrote new token to {}", ctx.token_file.display());

    println!("==> systemctl restart denia.service");
    systemd::restart("denia.service")?;
    println!("==> waiting up to 15s for denia.service is-active");
    systemd::wait_active("denia.service", Duration::from_secs(15))?;

    println!();
    println!("New admin token (capture now — printed once):");
    println!("  {token}");
    println!();
    println!(
        "Stored at {} (0640 {}:denia).",
        ctx.token_file.display(),
        ctx.install_user
    );
    Ok(())
}

fn write_token_file(ctx: &InstallContext, token: &str) -> anyhow::Result<()> {
    let body = format!("DENIA_ADMIN_TOKEN={token}\n");
    io::write_owned_secret(&ctx.token_file, &body, &ctx.install_user)
}
