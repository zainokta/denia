//! Filesystem I/O helpers shared across CLI subcommands.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Write a file atomically (tmp + chmod 0640 + chown <owner>:denia + rename).
/// Used for config, token, and age key.
pub fn write_owned_secret(path: &Path, body: &str, owner: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("no parent for {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("denia-tmp")
    ));
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o640))?;
    let owner_group = format!("{owner}:denia");
    let status = std::process::Command::new("chown")
        .args([&owner_group, &tmp_path.display().to_string()])
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "chown {owner_group} {} failed",
            tmp_path.display()
        ));
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}
