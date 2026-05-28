//! Privilege + installer-identity detection used by every privileged
//! subcommand. Refuses to run unless invoked via `sudo` from a non-root
//! account.

use anyhow::{Context, anyhow};
use std::path::PathBuf;
use std::process::Command;

use super::paths::InstallContext;

pub fn require_root() -> anyhow::Result<()> {
    // SAFETY: geteuid is signal-safe and never fails.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return Err(anyhow!(
            "this subcommand must run as root (try: sudo denia ...)"
        ));
    }
    Ok(())
}

pub fn detect_install_user() -> anyhow::Result<InstallContext> {
    let user = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")
        .ok_or_else(|| {
            anyhow!(
                "this subcommand must be invoked via sudo from a non-root account; \
                 SUDO_USER is unset or = 'root'"
            )
        })?;

    let home = resolve_home(&user)?;
    Ok(InstallContext::from_user(user, home))
}

fn resolve_home(user: &str) -> anyhow::Result<PathBuf> {
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .with_context(|| format!("invoking getent passwd {user}"))?;
    if !output.status.success() {
        return Err(anyhow!("getent passwd {user} returned non-zero"));
    }
    let line = String::from_utf8(output.stdout).context("getent output not utf-8")?;
    let home = line
        .trim_end_matches('\n')
        .split(':')
        .nth(5)
        .ok_or_else(|| anyhow!("malformed passwd line for {user}: {line:?}"))?
        .trim()
        .to_string();
    if home.is_empty() {
        return Err(anyhow!("empty or whitespace-only HOME for {user}"));
    }
    Ok(PathBuf::from(home))
}
