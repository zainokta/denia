//! systemd unit rendering. The systemctl wrappers land in Task 11.

use super::paths::InstallContext;
use sha2::Digest;

const TEMPLATE: &str = include_str!("../../templates/denia.service.in");

/// Render the operator-aware systemd unit text for `denia.service`.
pub fn render_unit(ctx: &InstallContext) -> String {
    TEMPLATE
        .replace("{{denia_bin}}", "/usr/local/bin/denia")
        .replace("{{denia_user}}", "denia")
        .replace("{{denia_group}}", "denia")
        .replace("{{denia_home}}", "/var/lib/denia")
        .replace("{{config_file}}", &ctx.config_file.display().to_string())
        .replace("{{age_key_file}}", &ctx.age_key_file.display().to_string())
        .replace("{{token_file}}", &ctx.token_file.display().to_string())
        .replace("{{user_config_dir}}", &ctx.user_config_dir.display().to_string())
}

/// SHA-256 of the rendered unit. Used by `denia doctor` (Task 16) to detect
/// hand-edited drift in `/etc/systemd/system/denia.service`.
pub fn unit_sha256(ctx: &InstallContext) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(render_unit(ctx).as_bytes());
    hasher.finalize().into()
}

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const UNIT_PATH: &str = "/etc/systemd/system/denia.service";

/// Write the rendered unit to `/etc/systemd/system/denia.service` via tmp +
/// rename (atomic) with mode `0644 root:root`.
pub fn write_unit(ctx: &InstallContext) -> anyhow::Result<()> {
    let body = render_unit(ctx);
    let tmp = format!("{UNIT_PATH}.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, UNIT_PATH)?;
    Ok(())
}

/// Read the on-disk unit at `/etc/systemd/system/denia.service`. Returns
/// `None` if the file is absent.
pub fn read_installed_unit() -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(UNIT_PATH) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn daemon_reload() -> anyhow::Result<()> {
    run("systemctl", &["daemon-reload"])
}

pub fn enable_now(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["enable", "--now", unit])
}

pub fn disable_now(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["disable", "--now", unit])
}

pub fn restart(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["restart", unit])
}

/// Returns true if the unit is in `active` state. Never errors — a missing
/// unit, a stopped unit, and an unreachable `systemctl` all return false.
pub fn is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll `is_active` every 500ms until either the unit is active or the
/// timeout expires.
pub fn wait_active(unit: &str, timeout: Duration) -> anyhow::Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if is_active(unit) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(anyhow::anyhow!(
        "{unit} did not become active within {}s",
        timeout.as_secs()
    ))
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("{bin} {args:?} exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_contains_required_directives() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let unit = render_unit(&ctx);
        for needle in [
            "User=denia",
            "Group=denia",
            "WorkingDirectory=/var/lib/denia",
            "Environment=DENIA_CONFIG_FILE=/home/rakei/.config/denia/config.toml",
            "Environment=SOPS_AGE_KEY_FILE=/home/rakei/.config/denia/age.key",
            "EnvironmentFile=/home/rakei/.config/denia/admin.token",
            "ExecStart=/usr/local/bin/denia",
            "BindReadOnlyPaths=/home/rakei/.config/denia",
            "AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN CAP_SETUID CAP_SETGID",
            "Delegate=yes",
            "ProtectHome=true",
            "Conflicts=traefik.service nginx.service caddy.service apache2.service httpd.service",
        ] {
            assert!(unit.contains(needle), "expected `{needle}` in unit:\n{unit}");
        }
    }

    #[test]
    fn unit_hash_is_stable_for_same_inputs() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        assert_eq!(unit_sha256(&ctx), unit_sha256(&ctx));
    }

    #[test]
    fn unit_hash_changes_per_install_context() {
        let a = InstallContext::from_user("rakei", "/home/rakei");
        let b = InstallContext::from_user("ops", "/home/ops");
        assert_ne!(unit_sha256(&a), unit_sha256(&b));
    }
}
