use std::path::PathBuf;

use thiserror::Error;

use crate::artifacts::ArtifactKind;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime lock poisoned")]
    LockPoisoned,
    #[error("invalid runtime service name: {name}")]
    InvalidServiceName { name: String },
    #[error("linux runtime requires a rootfs bundle artifact, got {kind:?}")]
    UnsupportedArtifactKind { kind: ArtifactKind },
    #[error("runtime process manifest is missing argv: {path}")]
    EmptyArgv { path: PathBuf },
    #[error("runtime process argv[0] must be an absolute path: {argv0}")]
    InvalidArgv { argv0: String },
    #[error("runtime process workdir must be absolute: {workdir}")]
    InvalidWorkdir { workdir: String },
    #[error("runtime process environment key is invalid: {key}")]
    InvalidEnvironmentKey { key: String },
    #[error("rootfs bundle is missing: {path}")]
    MissingRootfs { path: PathBuf },
    #[error("runtime path is unsafe: {path}")]
    UnsafeRuntimePath { path: PathBuf },
    #[error("invalid runtime resource limit: {reason}")]
    InvalidResourceLimit { reason: String },
    #[error("socket proxy binary not found: {path}")]
    SocketProxyUnavailable { path: PathBuf },
    #[error("service socket did not become ready: {path}")]
    ServiceSocketUnavailable { path: PathBuf },
    #[error("{action} failed at {path}: {source}")]
    PathIo {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest json error: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("syscall error: {0}")]
    Syscall(#[from] crate::syscall::SyscallError),
    #[error("native runtime wait task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}
