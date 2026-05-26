pub mod caps;
pub mod chown;
pub mod ns;
pub mod signal;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyscallError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("capability drop failed: {0}")]
    Capability(String),
    #[error("chown failed at {path}: {reason}")]
    Chown {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("namespace setup failed at {path}: {reason}")]
    NamespaceSetup {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("signal delivery failed for pid {pid}: {reason}")]
    Signal { pid: u32, reason: String },
}
