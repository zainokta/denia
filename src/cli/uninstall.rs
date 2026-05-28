//! `denia uninstall [--purge]`: tear down service. With --purge: also wipe
//! data + user config + system user.

use std::path::Path;
use std::process::{Command, Stdio};

use super::common::{paths::InstallContext, privilege, systemd};

#[derive(clap::Args, Debug)]
pub struct UninstallArgs {
    /// Also wipe /var/lib/denia, ~/.config/denia, and the denia system user.
    #[arg(long)]
    pub purge: bool,
    /// Print the plan without executing it.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: UninstallArgs) -> anyhow::Result<()> {
    privilege::require_root()?;
    // detect_install_user is required for `--purge` (to resolve ~/.config/denia).
    // Always call it so we can also surface a sensible error early. The
    // SUDO_USER env-var check inside detect_install_user covers the
    // sudo-invocation contract; without it we cannot know which operator's
    // home to clean.
    let ctx = privilege::detect_install_user()?;

    for step in plan(args.purge) {
        let label = step.label(&ctx);
        if args.dry_run {
            println!("[dry-run] {label}");
            continue;
        }
        println!("==> {label}");
        step.execute(&ctx)?;
    }

    if !args.dry_run {
        println!();
        println!("Denia service removed.");
        println!("  Remove the binary manually: sudo rm /usr/local/bin/denia");
        if !args.purge {
            println!(
                "  Data + config preserved. Re-run with --purge to wipe /var/lib/denia and ~/.config/denia."
            );
        }
    }
    Ok(())
}

fn plan(purge: bool) -> Vec<Step> {
    use Step::*;
    let mut steps = vec![SystemctlDisableNow, RemoveUnitFile, SystemctlDaemonReload];
    if purge {
        steps.extend([
            RemoveDataDir,
            RemoveUserConfigDir,
            UserDelDenia,
            GroupDelDenia,
            RmdirCgroupRoot,
        ]);
    }
    steps
}

enum Step {
    SystemctlDisableNow,
    RemoveUnitFile,
    SystemctlDaemonReload,
    RemoveDataDir,
    RemoveUserConfigDir,
    UserDelDenia,
    GroupDelDenia,
    RmdirCgroupRoot,
}

impl Step {
    fn label(&self, ctx: &InstallContext) -> String {
        use Step::*;
        match self {
            SystemctlDisableNow => {
                "systemctl disable --now denia.service (ignore if not loaded)".into()
            }
            RemoveUnitFile => "rm -f /etc/systemd/system/denia.service".into(),
            SystemctlDaemonReload => "systemctl daemon-reload".into(),
            RemoveDataDir => "rm -rf /var/lib/denia".into(),
            RemoveUserConfigDir => format!("rm -rf {}", ctx.user_config_dir.display()),
            UserDelDenia => "userdel denia".into(),
            GroupDelDenia => "groupdel denia".into(),
            RmdirCgroupRoot => "rmdir /sys/fs/cgroup/denia (best-effort)".into(),
        }
    }

    fn execute(&self, ctx: &InstallContext) -> anyhow::Result<()> {
        use Step::*;
        match self {
            SystemctlDisableNow => {
                // disable_now returns Err if the unit isn't loaded. Swallow
                // that case so re-running uninstall after the unit was
                // already removed is safe.
                let _ = systemd::disable_now("denia.service");
            }
            RemoveUnitFile => {
                let p = Path::new("/etc/systemd/system/denia.service");
                if p.exists() {
                    std::fs::remove_file(p)?;
                }
            }
            SystemctlDaemonReload => systemd::daemon_reload()?,
            RemoveDataDir => {
                let p = Path::new("/var/lib/denia");
                if p.exists() {
                    std::fs::remove_dir_all(p)?;
                }
            }
            RemoveUserConfigDir => {
                if ctx.user_config_dir.exists() {
                    std::fs::remove_dir_all(&ctx.user_config_dir)?;
                }
            }
            UserDelDenia => {
                // userdel exits 6 if the user doesn't exist; treat both 0 and 6 as success.
                let status = Command::new("userdel")
                    .arg("denia")
                    .stdin(Stdio::null())
                    .status()?;
                if !status.success() && status.code() != Some(6) {
                    return Err(anyhow::anyhow!("userdel denia exited with {status}"));
                }
            }
            GroupDelDenia => {
                let status = Command::new("groupdel")
                    .arg("denia")
                    .stdin(Stdio::null())
                    .status()?;
                if !status.success() && status.code() != Some(6) {
                    return Err(anyhow::anyhow!("groupdel denia exited with {status}"));
                }
            }
            RmdirCgroupRoot => {
                // Best-effort: ignore "directory not empty" or "no such file".
                let _ = std::fs::remove_dir("/sys/fs/cgroup/denia");
            }
        }
        Ok(())
    }
}
