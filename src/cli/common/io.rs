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
    super::provision::reject_symlink_components(parent)?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("denia-tmp");
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .tempfile_in(parent)?;
    {
        let f = tmp.as_file_mut();
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o640))?;
    let owner_group = format!("{owner}:denia");
    let status = std::process::Command::new("chown")
        .args([&owner_group, &tmp.path().display().to_string()])
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "chown {owner_group} {} failed",
            tmp.path().display()
        ));
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
