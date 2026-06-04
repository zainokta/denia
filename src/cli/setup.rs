//! `denia setup`: provision the host (idempotent).

use std::time::Duration;

use super::common::{
    config_writer, io, paths::InstallContext, privilege, provision, secrets, systemd,
};

#[derive(clap::Args, Debug)]
pub struct SetupArgs {
    /// Print the plan without executing it.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    privilege::require_root()?;
    let ctx = privilege::detect_install_user()?;

    for step in plan() {
        let label = step.label(&ctx);
        if args.dry_run {
            println!("[dry-run] {label}");
            continue;
        }
        println!("==> {label}");
        step.execute(&ctx)?;
    }

    if !args.dry_run {
        print_summary(&ctx);
    }
    Ok(())
}

/// Twelve idempotent setup steps. Plan order is the execution order — do not
/// reorder.
fn plan() -> Vec<Step> {
    use Step::*;
    vec![
        EnsureGroup,
        EnsureBuildkitGroup,
        EnsureUser,
        EnsureDeniaInBuildkitGroup,
        EnsureDataDirs,
        EnsureCgroupRoot,
        EnsureUserConfigDir,
        GenerateAgeIdentityIfAbsent,
        GenerateAdminTokenIfAbsent,
        WriteConfigIfAbsent,
        RepairUserConfigAccess,
        WriteBuildkitUnit,
        WriteSystemdUnit,
        SystemctlDaemonReload,
        SystemctlEnableBuildkitNow,
        SystemctlEnableNow,
        WaitActive,
    ]
}

enum Step {
    EnsureGroup,
    EnsureBuildkitGroup,
    EnsureUser,
    EnsureDeniaInBuildkitGroup,
    EnsureDataDirs,
    EnsureCgroupRoot,
    EnsureUserConfigDir,
    GenerateAgeIdentityIfAbsent,
    GenerateAdminTokenIfAbsent,
    WriteConfigIfAbsent,
    RepairUserConfigAccess,
    WriteBuildkitUnit,
    WriteSystemdUnit,
    SystemctlDaemonReload,
    SystemctlEnableBuildkitNow,
    SystemctlEnableNow,
    WaitActive,
}

impl Step {
    fn label(&self, ctx: &InstallContext) -> String {
        use Step::*;
        match self {
            EnsureGroup => "groupadd --system denia (if absent)".into(),
            EnsureBuildkitGroup => "groupadd --system buildkit (if absent)".into(),
            EnsureUser => "useradd --system denia (gid=denia, no home, nologin)".into(),
            EnsureDeniaInBuildkitGroup => "usermod -aG buildkit denia".into(),
            EnsureDataDirs => {
                "create /var/lib/denia/{sqlite,artifacts,tls,runtime,logs} 0700 denia:denia".into()
            }
            EnsureCgroupRoot => "create /sys/fs/cgroup/denia 0755 denia:denia".into(),
            EnsureUserConfigDir => format!(
                "create {} 0750 {}:denia",
                ctx.user_config_dir.display(),
                ctx.install_user
            ),
            GenerateAgeIdentityIfAbsent => format!(
                "generate age identity at {} 0640 {}:denia (if absent)",
                ctx.age_key_file.display(),
                ctx.install_user
            ),
            GenerateAdminTokenIfAbsent => format!(
                "generate admin token at {} 0640 {}:denia (if absent)",
                ctx.token_file.display(),
                ctx.install_user
            ),
            WriteConfigIfAbsent => format!(
                "write {} 0640 {}:denia (if absent)",
                ctx.config_file.display(),
                ctx.install_user
            ),
            RepairUserConfigAccess => format!(
                "repair {} file modes and denia traverse ACLs",
                ctx.user_config_dir.display()
            ),
            WriteBuildkitUnit => {
                "write /etc/systemd/system/buildkit.service (always overwrite)".into()
            }
            WriteSystemdUnit => "write /etc/systemd/system/denia.service (always overwrite)".into(),
            SystemctlDaemonReload => "systemctl daemon-reload".into(),
            SystemctlEnableBuildkitNow => "systemctl enable --now buildkit.service".into(),
            SystemctlEnableNow => "systemctl enable --now denia.service".into(),
            WaitActive => "wait up to 30s for systemctl is-active denia.service".into(),
        }
    }

    fn execute(&self, ctx: &InstallContext) -> anyhow::Result<()> {
        use Step::*;
        match self {
            EnsureGroup => {
                provision::ensure_group("denia")?;
            }
            EnsureBuildkitGroup => {
                provision::ensure_group("buildkit")?;
            }
            EnsureUser => {
                provision::ensure_user("denia", "denia", "/var/lib/denia")?;
            }
            EnsureDeniaInBuildkitGroup => {
                provision::ensure_user_in_group("denia", "buildkit")?;
            }
            EnsureDataDirs => provision::ensure_data_dirs()?,
            EnsureCgroupRoot => provision::ensure_cgroup_root()?,
            EnsureUserConfigDir => provision::ensure_user_config_dir(ctx)?,
            GenerateAgeIdentityIfAbsent => {
                if !ctx.age_key_file.exists() {
                    io::write_owned_secret(
                        &ctx.age_key_file,
                        &secrets::generate_age_identity(),
                        &ctx.install_user,
                    )?;
                }
            }
            GenerateAdminTokenIfAbsent => {
                if !ctx.token_file.exists() {
                    let token = secrets::generate_admin_token();
                    let body = format!("DENIA_ADMIN_TOKEN={token}\n");
                    io::write_owned_secret(&ctx.token_file, &body, &ctx.install_user)?;
                }
            }
            WriteConfigIfAbsent => {
                if !ctx.config_file.exists() {
                    let body = config_writer::render_config_toml(ctx);
                    io::write_owned_secret(&ctx.config_file, &body, &ctx.install_user)?;
                }
            }
            RepairUserConfigAccess => provision::repair_user_config_access(ctx)?,
            WriteBuildkitUnit => systemd::write_buildkit_unit()?,
            WriteSystemdUnit => systemd::write_unit(ctx)?,
            SystemctlDaemonReload => systemd::daemon_reload()?,
            SystemctlEnableBuildkitNow => systemd::enable_now("buildkit.service")?,
            SystemctlEnableNow => systemd::enable_now("denia.service")?,
            WaitActive => systemd::wait_active("denia.service", Duration::from_secs(30))?,
        }
        Ok(())
    }
}

fn print_summary(ctx: &InstallContext) {
    println!();
    println!("Denia is installed and running.");
    println!();
    println!("  config:       {}", ctx.config_file.display());
    println!("  admin token:  {}", ctx.token_file.display());
    println!("  age key:      {}", ctx.age_key_file.display());
    println!("  data root:    /var/lib/denia");
    println!();
    println!("  Bootstrap first admin user (one-time):");
    println!(
        "    TOKEN=\"$(sed -n 's/^DENIA_ADMIN_TOKEN=//p' {})\"",
        ctx.token_file.display()
    );
    println!("    curl -fsS -X POST -H \"Authorization: Bearer $TOKEN\" \\");
    println!("      -H 'Content-Type: application/json' \\");
    println!("      -d '{{\"username\":\"admin\",\"password\":\"<strong>\"}}' \\");
    println!("      http://127.0.0.1:7180/v1/bootstrap");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_plan_includes_buildkit_provisioning_before_denia_restart() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let labels = plan()
            .into_iter()
            .map(|step| step.label(&ctx))
            .collect::<Vec<_>>();

        let buildkit_group = labels
            .iter()
            .position(|label| label.contains("groupadd --system buildkit"))
            .expect("buildkit group step");
        let buildkit_unit = labels
            .iter()
            .position(|label| label.contains("/etc/systemd/system/buildkit.service"))
            .expect("buildkit unit step");
        let denia_unit = labels
            .iter()
            .position(|label| label.contains("/etc/systemd/system/denia.service"))
            .expect("denia unit step");

        assert!(buildkit_group < buildkit_unit);
        assert!(buildkit_unit < denia_unit);
    }
}
