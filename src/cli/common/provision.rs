//! User/group/directory provisioning used by `denia setup`. Every helper is
//! a probe-then-act idempotent operation so re-runs are safe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::paths::InstallContext;

/// Create the system group `name` if it doesn't already exist.
/// Returns `true` if it was created in this call, `false` if it already existed.
pub fn ensure_group(name: &str) -> anyhow::Result<bool> {
    if group_exists(name)? {
        return Ok(false);
    }
    run("groupadd", &["--system", name])?;
    Ok(true)
}

/// Create the system user `user` in group `group` with home `home`.
/// `no-create-home`, nologin shell. Returns true on create, false if it
/// already existed.
pub fn ensure_user(user: &str, group: &str, home: &str) -> anyhow::Result<bool> {
    if user_exists(user)? {
        return Ok(false);
    }
    run(
        "useradd",
        &[
            "--system",
            "--gid",
            group,
            "--home-dir",
            home,
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            user,
        ],
    )?;
    Ok(true)
}

/// Ensure a directory exists with the given mode, owner, and group. Idempotent.
/// Always re-applies mode + chown so a hand-edited install gets repaired.
pub fn ensure_dir(path: &Path, mode: u32, owner: &str, group: &str) -> anyhow::Result<()> {
    reject_symlink_components(path)?;
    std::fs::create_dir_all(path)?;
    reject_symlink_components(path)?;
    let p = path.display().to_string();
    run("chmod", &[&format!("{mode:o}"), &p])?;
    run("chown", &[&format!("{owner}:{group}"), &p])?;
    Ok(())
}

/// Ensure `/var/lib/denia` and its known subdirs (`sqlite/`, `artifacts/`,
/// `tls/`, `runtime/`, `logs/`) exist mode `0700 denia:denia`.
pub fn ensure_data_dirs() -> anyhow::Result<()> {
    let base = Path::new("/var/lib/denia");
    for sub in ["", "sqlite", "artifacts", "tls", "runtime", "logs"] {
        let p = if sub.is_empty() {
            base.to_path_buf()
        } else {
            base.join(sub)
        };
        ensure_dir(&p, 0o700, "denia", "denia")?;
    }
    Ok(())
}

/// Ensure `/sys/fs/cgroup/denia` exists mode `0755 denia:denia`. Required
/// for systemd cgroup v2 delegation to the unit.
pub fn ensure_cgroup_root() -> anyhow::Result<()> {
    ensure_dir(Path::new("/sys/fs/cgroup/denia"), 0o755, "denia", "denia")
}

/// Ensure the operator's `~/.config/denia` directory exists with the
/// installer owning the parent and `<installer>:denia` owning the denia
/// subdir (mode 0750). The daemon reads files in this dir via group bits;
/// see ADR-023 for the shared-group rationale.
pub fn ensure_user_config_dir(ctx: &InstallContext) -> anyhow::Result<()> {
    // ~/.config — usually exists; create if missing, leave alone if present.
    let parent = ctx.user_config_dir.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "user_config_dir has no parent: {}",
            ctx.user_config_dir.display()
        )
    })?;
    if !parent.exists() {
        ensure_dir(parent, 0o700, &ctx.install_user, &ctx.install_user)?;
    }
    // ~/.config/denia — operator-owned, denia group.
    ensure_dir(&ctx.user_config_dir, 0o750, &ctx.install_user, "denia")
}

pub(crate) fn reject_symlink_components(path: &Path) -> anyhow::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "refusing to follow symlinked path component: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn group_exists(name: &str) -> anyhow::Result<bool> {
    Ok(Command::new("getent")
        .args(["group", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn user_exists(name: &str) -> anyhow::Result<bool> {
    Ok(Command::new("getent")
        .args(["passwd", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin).args(args).stdin(Stdio::null()).status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("{bin} {args:?} exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn reject_symlink_components_rejects_final_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = reject_symlink_components(&link).unwrap_err();

        assert!(err.to_string().contains("symlinked path component"));
    }

    #[cfg(unix)]
    #[test]
    fn reject_symlink_components_rejects_parent_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = reject_symlink_components(&link.join("denia")).unwrap_err();

        assert!(err.to_string().contains("symlinked path component"));
    }
}
