use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::Utc;
use uuid::Uuid;

/// Resolve the per-deployment log path used by the writer, SSE handler, and
/// orphan-recovery synthetic line.
pub fn deployment_log_path(log_dir: &Path, deployment_id: Uuid) -> PathBuf {
    log_dir
        .join("deployments")
        .join(format!("{deployment_id}.log"))
}

#[derive(Debug)]
pub struct DeploymentLogWriter {
    path: PathBuf,
    handle: Mutex<std::fs::File>,
}

impl DeploymentLogWriter {
    pub fn create(log_dir: &Path, deployment_id: Uuid) -> std::io::Result<Self> {
        let path = deployment_log_path(log_dir, deployment_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let handle = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path,
            handle: Mutex::new(handle),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, phase: &str, message: &str) -> std::io::Result<()> {
        let ts = Utc::now().to_rfc3339();
        let line = format!("{ts} {phase} {message}\n");
        let mut g = self.handle.lock().expect("log writer mutex poisoned");
        g.write_all(line.as_bytes())?;
        g.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn writes_one_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let w = DeploymentLogWriter::create(dir.path(), id).unwrap();
        w.write("OCI_PULL", "starting").unwrap();
        w.write("OCI_PULL", "done").unwrap();
        let body = std::fs::read_to_string(w.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("OCI_PULL starting"));
        assert!(lines[1].contains("OCI_PULL done"));
    }

    #[test]
    fn path_is_under_log_dir_deployments_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let p = deployment_log_path(dir.path(), id);
        assert_eq!(p.parent().unwrap(), dir.path().join("deployments"));
        assert_eq!(p.extension().unwrap(), "log");
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let w = DeploymentLogWriter::create(dir.path(), id).unwrap();
        w.write("BOOT", "ok").unwrap();
        let mode = std::fs::metadata(w.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
