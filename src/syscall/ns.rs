use std::path::PathBuf;

use crate::syscall::SyscallError;

/// Configuration for a namespaced one-shot or long-running process launch.
///
/// `NamespaceConfig` is the in-process counterpart to the `unshare` CLI flags.
/// Building this struct describes *what* to unshare and *how* to map uids; the
/// actual fork + unshare + uid_map write happens in `spawn_namespaced_process`.
///
/// The struct is intentionally a builder around concrete fields so its public
/// surface is testable without needing root privileges. Privileged execution
/// is gated to `spawn_namespaced_process` which returns
/// `SyscallError::Capability` on non-root callers.
#[derive(Debug, Clone)]
pub struct NamespaceConfig {
    pub rootfs: PathBuf,
    pub workdir: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub userns: bool,
    pub uid_map: Option<UidMap>,
    pub gid_map: Option<UidMap>,
    pub pid_ns: bool,
    pub net_ns: bool,
    pub mount_ns: bool,
    pub uts_ns: bool,
    pub ipc_ns: bool,
    pub mount_proc: bool,
    pub no_new_privs: bool,
    pub drop_bounding_caps: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidMap {
    pub inside: u32,
    pub outside: u32,
    pub size: u32,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            rootfs: PathBuf::new(),
            workdir: "/".to_string(),
            argv: Vec::new(),
            env: Vec::new(),
            userns: true,
            uid_map: None,
            gid_map: None,
            pid_ns: true,
            net_ns: true,
            mount_ns: true,
            uts_ns: true,
            ipc_ns: true,
            mount_proc: true,
            no_new_privs: true,
            drop_bounding_caps: true,
        }
    }
}

impl NamespaceConfig {
    pub fn new(rootfs: impl Into<PathBuf>, argv: Vec<String>) -> Self {
        Self {
            rootfs: rootfs.into(),
            argv,
            ..Self::default()
        }
    }

    pub fn with_uid_map(mut self, base: u32, size: u32) -> Self {
        self.uid_map = Some(UidMap {
            inside: 0,
            outside: base,
            size,
        });
        self.gid_map = Some(UidMap {
            inside: 0,
            outside: base,
            size,
        });
        self
    }

    pub fn validate(&self) -> Result<(), SyscallError> {
        if self.argv.is_empty() {
            return Err(SyscallError::Capability(
                "argv must contain the program path".to_string(),
            ));
        }
        if !self.rootfs.is_absolute() {
            return Err(SyscallError::Capability(format!(
                "rootfs must be absolute, got {}",
                self.rootfs.display()
            )));
        }
        if !self.workdir.starts_with('/') {
            return Err(SyscallError::Capability(format!(
                "workdir must be absolute, got {}",
                self.workdir
            )));
        }
        Ok(())
    }
}

/// Fork + unshare + apply uid_map + exec argv. Returns the child pid.
///
/// **Privileged**: requires `CAP_SYS_ADMIN` (or root) for namespace creation
/// when `userns=false`; usable unprivileged with `userns=true` on kernels
/// with unprivileged userns enabled. Returns `SyscallError::Capability` on
/// non-Linux platforms or when the calling context lacks the required
/// capabilities. Full implementation lands in a privileged-runtime PR.
pub fn spawn_namespaced_process(_config: &NamespaceConfig) -> Result<u32, SyscallError> {
    Err(SyscallError::Capability(
        "spawn_namespaced_process: privileged in-process namespace launch not yet implemented; \
         LinuxRuntime currently uses the `unshare` CLI launcher (ADR-005). This is scaffold."
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enables_full_isolation() {
        let cfg = NamespaceConfig::default();
        assert!(cfg.userns);
        assert!(cfg.pid_ns);
        assert!(cfg.net_ns);
        assert!(cfg.mount_ns);
        assert!(cfg.no_new_privs);
        assert!(cfg.drop_bounding_caps);
    }

    #[test]
    fn with_uid_map_sets_both_uid_and_gid() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536);
        assert_eq!(
            cfg.uid_map,
            Some(UidMap {
                inside: 0,
                outside: 100000,
                size: 65536,
            })
        );
        assert_eq!(
            cfg.gid_map,
            Some(UidMap {
                inside: 0,
                outside: 100000,
                size: 65536
            })
        );
    }

    #[test]
    fn validate_rejects_empty_argv() {
        let mut cfg = NamespaceConfig::default();
        cfg.rootfs = "/var/lib/denia/rootfs".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_relative_rootfs() {
        let cfg = NamespaceConfig::new("relative/path", vec!["/bin/true".to_string()]);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn spawn_returns_capability_error_until_privileged_impl_lands() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()]);
        let err = spawn_namespaced_process(&cfg).unwrap_err();
        match err {
            SyscallError::Capability(_) => {}
            other => panic!("expected Capability, got {other:?}"),
        }
    }
}
