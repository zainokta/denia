//! User/group/directory provisioning used by `denia setup`. Every helper is
//! a probe-then-act idempotent operation so re-runs are safe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::os::unix::fs::PermissionsExt;

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

/// Ensure `user` is a member of `group`. Idempotent via `usermod -aG`.
pub fn ensure_user_in_group(user: &str, group: &str) -> anyhow::Result<()> {
    run("usermod", &["-aG", group, user])
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
    let cgroup_root = Path::new("/sys/fs/cgroup/denia");
    ensure_dir(cgroup_root, 0o755, "denia", "denia")?;
    ensure_cgroup_controllers(cgroup_root, &["cpu", "memory", "pids", "io"])?;
    Ok(())
}

fn ensure_cgroup_controllers(path: &Path, controllers: &[&str]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        enable_available_cgroup_controllers(parent, controllers)?;
    }
    enable_available_cgroup_controllers(path, controllers)
}

fn enable_available_cgroup_controllers(path: &Path, controllers: &[&str]) -> anyhow::Result<()> {
    let controllers_path = path.join("cgroup.controllers");
    let subtree_control_path = path.join("cgroup.subtree_control");
    if !controllers_path.exists() || !subtree_control_path.exists() {
        return Ok(());
    }

    let available = std::fs::read_to_string(&controllers_path)?;
    let enabled = std::fs::read_to_string(&subtree_control_path)?;
    let missing = controllers
        .iter()
        .filter(|controller| {
            available
                .split_whitespace()
                .any(|available| available == **controller)
        })
        .filter(|controller| {
            !enabled
                .split_whitespace()
                .any(|enabled| enabled == **controller)
        })
        .map(|controller| format!("+{controller}"))
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(());
    }

    std::fs::write(&subtree_control_path, format!("{}\n", missing.join(" "))).map_err(|error| {
        anyhow::anyhow!(
            "enable cgroup controllers at {} failed: {error}",
            subtree_control_path.display()
        )
    })
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

/// Repair all access needed for the `denia` system user to read the operator
/// config directory created by `denia setup`.
pub fn repair_user_config_access(ctx: &InstallContext) -> anyhow::Result<()> {
    for repair in user_config_access_repair_plan(ctx) {
        apply_access_repair(&repair)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum AccessRepair {
    GrantUserExecute {
        path: PathBuf,
        user: String,
    },
    EnsureDir {
        path: PathBuf,
        mode: u32,
        owner: String,
        group: String,
    },
    EnsureFile {
        path: PathBuf,
        mode: u32,
        owner: String,
        group: String,
    },
}

fn user_config_access_repair_plan(ctx: &InstallContext) -> Vec<AccessRepair> {
    let config_parent = ctx
        .user_config_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ctx.install_home.join(".config"));
    vec![
        AccessRepair::GrantUserExecute {
            path: ctx.install_home.clone(),
            user: "denia".to_string(),
        },
        AccessRepair::GrantUserExecute {
            path: config_parent,
            user: "denia".to_string(),
        },
        AccessRepair::EnsureDir {
            path: ctx.user_config_dir.clone(),
            mode: 0o750,
            owner: ctx.install_user.clone(),
            group: "denia".to_string(),
        },
        AccessRepair::EnsureFile {
            path: ctx.config_file.clone(),
            mode: 0o640,
            owner: ctx.install_user.clone(),
            group: "denia".to_string(),
        },
        AccessRepair::EnsureFile {
            path: ctx.token_file.clone(),
            mode: 0o640,
            owner: ctx.install_user.clone(),
            group: "denia".to_string(),
        },
        AccessRepair::EnsureFile {
            path: ctx.age_key_file.clone(),
            mode: 0o640,
            owner: ctx.install_user.clone(),
            group: "denia".to_string(),
        },
    ]
}

fn apply_access_repair(repair: &AccessRepair) -> anyhow::Result<()> {
    match repair {
        AccessRepair::GrantUserExecute { path, user } => {
            if path.exists() {
                reject_symlink_components(path)?;
                let p = path.display().to_string();
                run("setfacl", &["-m", &format!("u:{user}:x"), &p])?;
            }
        }
        AccessRepair::EnsureDir {
            path,
            mode,
            owner,
            group,
        } => ensure_dir(path, *mode, owner, group)?,
        AccessRepair::EnsureFile {
            path,
            mode,
            owner,
            group,
        } => {
            if path.exists() {
                reject_symlink_components(path)?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))?;
                let p = path.display().to_string();
                run("chown", &[&format!("{owner}:{group}"), &p])?;
            }
        }
    }
    Ok(())
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

    #[test]
    fn user_config_access_repair_plan_repairs_existing_paths() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");

        let plan = user_config_access_repair_plan(&ctx);

        assert_eq!(
            plan,
            vec![
                AccessRepair::GrantUserExecute {
                    path: PathBuf::from("/home/rakei"),
                    user: "denia".to_string(),
                },
                AccessRepair::GrantUserExecute {
                    path: PathBuf::from("/home/rakei/.config"),
                    user: "denia".to_string(),
                },
                AccessRepair::EnsureDir {
                    path: PathBuf::from("/home/rakei/.config/denia"),
                    mode: 0o750,
                    owner: "rakei".to_string(),
                    group: "denia".to_string(),
                },
                AccessRepair::EnsureFile {
                    path: PathBuf::from("/home/rakei/.config/denia/config.toml"),
                    mode: 0o640,
                    owner: "rakei".to_string(),
                    group: "denia".to_string(),
                },
                AccessRepair::EnsureFile {
                    path: PathBuf::from("/home/rakei/.config/denia/admin.token"),
                    mode: 0o640,
                    owner: "rakei".to_string(),
                    group: "denia".to_string(),
                },
                AccessRepair::EnsureFile {
                    path: PathBuf::from("/home/rakei/.config/denia/age.key"),
                    mode: 0o640,
                    owner: "rakei".to_string(),
                    group: "denia".to_string(),
                },
            ]
        );
    }

    #[test]
    fn enable_available_cgroup_controllers_skips_already_enabled_readonly_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cgroup");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("cgroup.controllers"), "cpu memory pids io\n").unwrap();
        let subtree_control = root.join("cgroup.subtree_control");
        std::fs::write(&subtree_control, "cpu memory pids io\n").unwrap();
        std::fs::set_permissions(&subtree_control, std::fs::Permissions::from_mode(0o444)).unwrap();

        enable_available_cgroup_controllers(&root, &["cpu", "memory", "pids", "io"]).unwrap();

        assert_eq!(
            std::fs::read_to_string(&subtree_control).unwrap(),
            "cpu memory pids io\n"
        );
    }

    #[test]
    fn enable_available_cgroup_controllers_writes_only_available_missing_controllers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cgroup");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("cgroup.controllers"), "cpu memory\n").unwrap();
        let subtree_control = root.join("cgroup.subtree_control");
        std::fs::write(&subtree_control, "cpu\n").unwrap();

        enable_available_cgroup_controllers(&root, &["cpu", "memory", "pids", "io"]).unwrap();

        assert_eq!(
            std::fs::read_to_string(&subtree_control).unwrap(),
            "+memory\n"
        );
    }

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
