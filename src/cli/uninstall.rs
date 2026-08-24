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
        println!("  Remove binaries manually: sudo rm /usr/local/bin/denia /usr/local/bin/zot");
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
        SystemctlDisableZotNow,
        RemoveUnitFile,
        RemoveZotUnitFile,
        SystemctlDaemonReload,
    ];
    if purge {
        steps.extend([
            RemoveDataDir,
            RemoveZotConfig,
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
    SystemctlDisableZotNow,
    RemoveUnitFile,
    RemoveZotUnitFile,
    SystemctlDaemonReload,
    RemoveDataDir,
    RemoveZotConfig,
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
            SystemctlDisableZotNow => {
                "systemctl disable --now zot.service (ignore if not loaded)".into()
            }
            RemoveUnitFile => "rm -f /etc/systemd/system/denia.service".into(),
            RemoveZotUnitFile => "rm -f /etc/systemd/system/zot.service".into(),
            SystemctlDaemonReload => "systemctl daemon-reload".into(),
            RemoveDataDir => "rm -rf /var/lib/denia".into(),
            RemoveZotConfig => "rm -f /etc/denia/zot.json (and empty /etc/denia)".into(),
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
                let _ = systemd::disable_now("denia.service");
            }
            SystemctlDisableZotNow => {
                let _ = systemd::disable_now("zot.service");
            }
            RemoveUnitFile => remove_if_exists("/etc/systemd/system/denia.service")?,
            RemoveZotUnitFile => remove_if_exists("/etc/systemd/system/zot.service")?,
            SystemctlDaemonReload => systemd::daemon_reload()?,
            RemoveDataDir => {
                let p = Path::new("/var/lib/denia");
                if p.exists() {
                    std::fs::remove_dir_all(p)?;
                }
            }
            RemoveZotConfig => {
                remove_if_exists("/etc/denia/zot.json")?;
                let _ = std::fs::remove_dir("/etc/denia");
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
        let stop_zot = labels.iter().position(|v| v.contains("disable --now zot.service")).unwrap();
        let remove_zot = labels.iter().position(|v| v.contains("zot.service") && v.contains("rm -f")).unwrap();
        assert!(stop_zot < remove_zot);
    }
}
