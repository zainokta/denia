//! `denia uninstall [--purge]`: tear down service. With --purge: also wipe
//! data + user config + system user.

use std::path::Path;
use std::process::{Command, Stdio};

use super::common::{paths::InstallContext, privilege, systemd, zot};

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
        println!("  Remove binaries manually: sudo rm /usr/local/bin/denia");
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
    let mut steps = vec![
        SystemctlDisableNow,
        RemoveManagedZot { purge },
        RemoveUnitFile,
        SystemctlDaemonReload,
    ];
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
    RemoveManagedZot { purge: bool },
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
            RemoveManagedZot { purge } => format!(
                "stop and remove only Denia-managed Zot service + dependency drop-in (purge={purge})"
            ),
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
            SystemctlDisableNow => systemd::disable_if_present("denia.service")?,
            RemoveManagedZot { purge } => zot::remove_managed_service(*purge)?,
            RemoveUnitFile => remove_if_exists("/etc/systemd/system/denia.service")?,
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
                let _ = std::fs::remove_dir("/sys/fs/cgroup/denia");
            }
        }
        Ok(())
    }
}

fn remove_if_exists(path: &str) -> anyhow::Result<()> {
    let p = Path::new(path);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_stops_zot_before_removing_units() {
        let ctx = InstallContext::from_user("ops", "/home/ops");
        let labels = plan(false)
            .into_iter()
            .map(|step| step.label(&ctx))
            .collect::<Vec<_>>();
        let remove_zot = labels
            .iter()
            .position(|v| v.contains("only Denia-managed Zot"))
            .unwrap();
        let remove_denia = labels
            .iter()
            .position(|v| v.contains("rm -f /etc/systemd/system/denia.service"))
            .unwrap();
        assert!(remove_zot < remove_denia);
    }
}
