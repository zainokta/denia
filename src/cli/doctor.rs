//! `denia doctor`: diagnose host + denia install. No privilege required;
//! root-only checks SKIP gracefully when run unprivileged.

use std::process::Command;
use std::time::Duration;

use crate::config::{FileConfig, config_file_path};

use super::common::paths::InstallContext;
use super::common::platform;
use super::common::systemd;

const CGROUP_CONTROLLERS: &[&str] = &["cpu", "memory", "pids", "io"];
const REQUIRED_CGROUP_CONTROLLERS: &[&str] = &["cpu", "memory", "pids"];

pub enum CheckResult {
    Pass(String),
    Fail(String),
    Skip(String),
}

pub fn run() -> anyhow::Result<()> {
    // Try to detect install context; if SUDO_USER is missing, some checks skip.
    let ctx = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")
        .and_then(|user| {
            // Try getent passwd <user> | cut -d: -f6.
            let out = Command::new("getent")
                .args(["passwd", &user])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let line = String::from_utf8(out.stdout).ok()?;
            let home = line
                .trim_end_matches('\n')
                .split(':')
                .nth(5)?
                .trim()
                .to_string();
            if home.is_empty() {
                return None;
            }
            Some(InstallContext::from_user(user, home))
        });

    let checks: Vec<CheckResult> = vec![
        check_glibc_baseline(),
        check_cgroup_v2(),
        check_denia_cgroup_delegation(),
        check_userns_enabled(),
        check_ports_free(),
        check_deps_in_path(),
        check_user_config_files(ctx.as_ref()),
        check_unit_drift(ctx.as_ref()),
        check_healthz(),
    ];

    let mut any_fail = false;
    for result in &checks {
        match result {
            CheckResult::Pass(msg) => println!("[PASS] {msg}"),
            CheckResult::Fail(msg) => {
                println!("[FAIL] {msg}");
                any_fail = true;
            }
            CheckResult::Skip(msg) => println!("[SKIP] {msg}"),
        }
    }

    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

fn check_glibc_baseline() -> CheckResult {
    match platform::host_glibc_version() {
        Ok(version) if platform::is_supported_glibc_version(version) => CheckResult::Pass(format!(
            "glibc {} meets release baseline >= {}",
            platform::format_glibc_version(version),
            platform::minimum_glibc_label()
        )),
        Ok(version) => CheckResult::Fail(format!(
            "glibc {} is below required {}; upgrade the host OS before using signed release updates",
            platform::format_glibc_version(version),
            platform::minimum_glibc_label()
        )),
        Err(e) => CheckResult::Fail(format!("glibc baseline check failed: {e}")),
    }
}

fn check_cgroup_v2() -> CheckResult {
    let mounts = match std::fs::read_to_string("/proc/mounts") {
        Ok(s) => s,
        Err(e) => return CheckResult::Fail(format!("cgroup v2: cannot read /proc/mounts ({e})")),
    };
    if mounts
        .lines()
        .any(|l| l.starts_with("cgroup2 /sys/fs/cgroup cgroup2 "))
    {
        CheckResult::Pass("cgroup v2 unified mount at /sys/fs/cgroup".into())
    } else {
        CheckResult::Fail(
            "cgroup v2 unified mount missing; boot with systemd.unified_cgroup_hierarchy=1".into(),
        )
    }
}

fn check_denia_cgroup_delegation() -> CheckResult {
    let cgroup_root = denia_cgroup_root_for_doctor();
    if !cgroup_root.exists() {
        return CheckResult::Fail(format!(
            "denia cgroup root missing at {}; run `sudo denia setup`, then `sudo systemctl restart denia`",
            cgroup_root.display()
        ));
    }

    let controllers_path = cgroup_root.join("cgroup.controllers");
    let subtree_control_path = cgroup_root.join("cgroup.subtree_control");
    let available = match std::fs::read_to_string(&controllers_path) {
        Ok(raw) => raw,
        Err(error) => {
            return CheckResult::Fail(format!(
                "denia cgroup delegation: cannot read {} ({error})",
                controllers_path.display()
            ));
        }
    };
    let enabled = match std::fs::read_to_string(&subtree_control_path) {
        Ok(raw) => raw,
        Err(error) => {
            return CheckResult::Fail(format!(
                "denia cgroup delegation: cannot read {} ({error})",
                subtree_control_path.display()
            ));
        }
    };

    let unavailable_required =
        unavailable_cgroup_controllers(&available, REQUIRED_CGROUP_CONTROLLERS);
    if !unavailable_required.is_empty() {
        return CheckResult::Fail(format!(
            "denia cgroup delegation: required controller(s) unavailable at {}: {}",
            controllers_path.display(),
            unavailable_required.join(", ")
        ));
    }

    let missing_enabled = missing_cgroup_controllers(&available, &enabled, CGROUP_CONTROLLERS);
    if missing_enabled.is_empty() {
        CheckResult::Pass(format!(
            "denia cgroup delegation enabled for {}",
            enabled.split_whitespace().collect::<Vec<_>>().join(", ")
        ))
    } else {
        CheckResult::Fail(format!(
            "denia cgroup delegation: controller(s) available but not enabled under {}: {}. Re-run `sudo denia setup`, then `sudo systemctl restart denia`.",
            cgroup_root.display(),
            missing_enabled.join(", ")
        ))
    }
}

fn denia_cgroup_root_for_doctor() -> std::path::PathBuf {
    Command::new("systemctl")
        .args(["show", "denia.service", "-p", "ControlGroup", "--value"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|raw| systemd_delegated_cgroup_root(raw.trim()))
        .unwrap_or_else(|| std::path::PathBuf::from("/sys/fs/cgroup/denia"))
}

fn systemd_delegated_cgroup_root(current: &str) -> Option<std::path::PathBuf> {
    if current == "/" || !current.starts_with('/') {
        return None;
    }

    let mut path = std::path::PathBuf::from("/sys/fs/cgroup");
    for component in current.trim_start_matches('/').split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None;
        }
        path.push(component);
    }
    path.push("denia");
    Some(path)
}

fn missing_cgroup_controllers(available: &str, enabled: &str, controllers: &[&str]) -> Vec<String> {
    controllers
        .iter()
        .filter(|controller| {
            available
                .split_whitespace()
                .any(|available| available == **controller)
                && !enabled
                    .split_whitespace()
                    .any(|enabled| enabled == **controller)
        })
        .map(|controller| (*controller).to_string())
        .collect()
}

fn unavailable_cgroup_controllers(available: &str, controllers: &[&str]) -> Vec<String> {
    controllers
        .iter()
        .filter(|controller| {
            !available
                .split_whitespace()
                .any(|available| available == **controller)
        })
        .map(|controller| (*controller).to_string())
        .collect()
}

fn check_userns_enabled() -> CheckResult {
    let path = "/proc/sys/user/max_user_namespaces";
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return CheckResult::Fail(format!("user namespaces: cannot read {path} ({e})")),
    };
    let val: u64 = raw.trim().parse().unwrap_or(0);
    if val > 0 {
        CheckResult::Pass(format!("user namespaces enabled (max={val})"))
    } else {
        CheckResult::Fail(format!("user namespaces disabled (max={val})"))
    }
}

fn check_ports_free() -> CheckResult {
    let out = match Command::new("ss")
        .args(["-ltnH", "( sport = :80 or sport = :443 )"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return CheckResult::Skip("ss(8) not available; install iproute2".into()),
    };
    if !out.status.success() {
        return CheckResult::Skip("ss returned non-zero; skipping".into());
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let trimmed = listing.trim();
    if trimmed.is_empty() {
        CheckResult::Pass(":80 and :443 free (or owned by denia.service)".into())
    } else {
        CheckResult::Fail(format!(
            ":80/:443 already bound; conflicting listeners:\n{trimmed}"
        ))
    }
}

fn check_deps_in_path() -> CheckResult {
    let deps = ["age", "sops", "git", "buildctl"];
    let missing: Vec<&str> = deps
        .iter()
        .copied()
        .filter(|bin| which(bin).is_none())
        .collect();
    if missing.is_empty() {
        CheckResult::Pass(format!("deps in PATH: {}", deps.join(", ")))
    } else {
        CheckResult::Fail(format!("missing deps in PATH: {}", missing.join(", ")))
    }
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    // First try the `which` binary; fall back to a manual $PATH scan so that
    // environments without `which` (busybox, minimal containers) still work.
    if let Ok(out) = Command::new("which").arg(bin).output()
        && out.status.success()
    {
        let line = String::from_utf8(out.stdout).unwrap_or_default();
        let p = line.trim();
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    // Manual PATH scan fallback.
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn check_user_config_files(ctx: Option<&InstallContext>) -> CheckResult {
    let ctx = match ctx {
        Some(c) => c,
        None => {
            return CheckResult::Skip(
                "operator config files (set SUDO_USER or run via sudo to check)".into(),
            );
        }
    };

    let expected = [
        (&ctx.config_file, "config.toml"),
        (&ctx.token_file, "admin.token"),
        (&ctx.age_key_file, "age.key"),
    ];

    let mut problems: Vec<String> = Vec::new();
    for (path, label) in expected.iter() {
        if !path.exists() {
            problems.push(format!("{label} missing at {}", path.display()));
            continue;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("{label} stat() failed: {e}"));
                continue;
            }
        };
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o640 {
            problems.push(format!(
                "{label} mode {mode:o} (expected 640) at {}",
                path.display()
            ));
        }
        // Check group ownership against the `denia` group GID.
        if let Some(denia_gid) = denia_gid()
            && meta.gid() != denia_gid
        {
            problems.push(format!(
                "{label} gid {} (expected denia gid {denia_gid}) at {}",
                meta.gid(),
                path.display()
            ));
        }
    }

    if problems.is_empty() {
        CheckResult::Pass(format!(
            "operator config files at {} are 0640 with denia group",
            ctx.user_config_dir.display()
        ))
    } else {
        CheckResult::Fail(format!(
            "operator config files: {} problem(s):\n  - {}",
            problems.len(),
            problems.join("\n  - ")
        ))
    }
}

fn denia_gid() -> Option<u32> {
    let out = Command::new("getent")
        .args(["group", "denia"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?;
    line.trim_end_matches('\n').split(':').nth(2)?.parse().ok()
}

fn check_unit_drift(ctx: Option<&InstallContext>) -> CheckResult {
    let ctx = match ctx {
        Some(c) => c,
        None => {
            return CheckResult::Skip(
                "systemd unit drift (set SUDO_USER or run via sudo to check)".into(),
            );
        }
    };
    let on_disk = match systemd::read_installed_unit() {
        Ok(Some(s)) => s,
        Ok(None) => {
            return CheckResult::Fail(
                "/etc/systemd/system/denia.service is not installed; run `sudo denia setup`".into(),
            );
        }
        Err(e) => return CheckResult::Fail(format!("read denia.service failed: {e}")),
    };
    let expected = systemd::render_unit(ctx);
    if on_disk == expected {
        CheckResult::Pass("systemd unit matches template (no drift)".into())
    } else {
        CheckResult::Fail(
            "systemd unit drift: /etc/systemd/system/denia.service != template render. \
             Re-run `sudo denia setup` to refresh."
                .into(),
        )
    }
}

fn check_healthz() -> CheckResult {
    let config_path = config_file_path();
    let bind_addr = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|raw| toml::from_str::<FileConfig>(&raw).ok())
            .and_then(|c| c.bind_addr)
            .unwrap_or_else(|| "127.0.0.1:7180".to_string())
    } else {
        "127.0.0.1:7180".to_string()
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return CheckResult::Fail(format!("healthz: tokio runtime build failed ({e})"));
        }
    };
    let status = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        client
            .get(format!("http://{bind_addr}/healthz"))
            .send()
            .await
            .map(|r| r.status())
    });
    match status {
        Ok(code) if code.is_success() => CheckResult::Pass(format!(
            "/healthz reachable at http://{bind_addr}/healthz ({code})"
        )),
        Ok(code) => CheckResult::Fail(format!(
            "/healthz returned {code} from http://{bind_addr}/healthz"
        )),
        Err(e) => CheckResult::Fail(format!(
            "/healthz unreachable at http://{bind_addr}/healthz ({e})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cgroup_controllers_reports_available_but_disabled() {
        assert_eq!(
            missing_cgroup_controllers("cpu memory pids io\n", "cpu pids\n", CGROUP_CONTROLLERS),
            vec!["memory".to_string(), "io".to_string()]
        );
    }

    #[test]
    fn missing_cgroup_controllers_ignores_unavailable_optional_controller() {
        assert_eq!(
            missing_cgroup_controllers(
                "cpu memory pids\n",
                "cpu memory pids\n",
                CGROUP_CONTROLLERS
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unavailable_cgroup_controllers_reports_required_absent_from_available_set() {
        assert_eq!(
            unavailable_cgroup_controllers("cpu pids io\n", REQUIRED_CGROUP_CONTROLLERS),
            vec!["memory".to_string()]
        );
    }

    #[test]
    fn systemd_delegated_cgroup_root_uses_service_control_group() {
        assert_eq!(
            systemd_delegated_cgroup_root("/system.slice/denia.service").unwrap(),
            std::path::PathBuf::from("/sys/fs/cgroup/system.slice/denia.service/denia")
        );
    }
}
