//! `denia doctor`: diagnose host + denia install. No privilege required;
//! root-only checks SKIP gracefully when run unprivileged.

use std::process::Command;
use std::time::Duration;

use crate::config::{FileConfig, config_file_path};

use super::common::paths::InstallContext;
use super::common::systemd;

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
        check_cgroup_v2(),
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
