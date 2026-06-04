//! Runtime-facing console session types. The API layer builds a
//! [`RuntimeConsoleRequest`] and the runtime returns a [`RuntimeConsoleSession`]
//! carrying a live PTY bound to the target replica. See ADR-033.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use uuid::Uuid;

/// What the runtime needs to open a console against a tracked replica.
#[derive(Debug, Clone)]
pub struct RuntimeConsoleRequest {
    pub session_id: Uuid,
    pub service_id: Uuid,
    pub service_name: String,
    pub deployment_id: Uuid,
    pub replica_index: u32,
    pub cols: u16,
    pub rows: u16,
}

/// How the console shell ended, for the `exit` control frame + audit log
/// (ADR-033 "exit reason"). `None`-ish ambiguity is captured explicitly so the
/// audit trail can distinguish "exited 0" from "we never learned".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleExit {
    /// Shell exited normally with this status code.
    Code(i32),
    /// Shell was terminated by a signal (e.g. SIGTERM/SIGKILL on teardown).
    Signal(i32),
    /// The exit reason could not be determined (already reaped elsewhere, wait
    /// failed, or runtime does not track it).
    Unknown,
}

impl ConsoleExit {
    /// The numeric code for the protocol `exit` frame (`{"type":"exit","code":N}`),
    /// or `None` when the reason is unknown.
    pub fn frame_code(&self) -> Option<i32> {
        match self {
            ConsoleExit::Code(code) => Some(*code),
            // Convention: a signal N surfaces as 128+N, matching shells.
            ConsoleExit::Signal(sig) => Some(128 + *sig),
            ConsoleExit::Unknown => None,
        }
    }

    /// Stable human label for the audit log "session end" record.
    pub fn audit_reason(&self) -> String {
        match self {
            ConsoleExit::Code(code) => format!("exit code {code}"),
            ConsoleExit::Signal(sig) => format!("signal {sig}"),
            ConsoleExit::Unknown => "unknown".to_string(),
        }
    }
}

/// Tears down a console child: signals it (SIGTERM, escalating to SIGKILL after
/// a grace period), reaps it, and reports how it ended. Held as a trait object
/// so the runtime-agnostic [`RuntimeConsoleSession`] can drive teardown without
/// the API layer knowing about pids/signals. The fake runtime supplies a no-op.
#[async_trait]
pub trait ConsoleReaper: Send + Sync {
    async fn reap(&self) -> ConsoleExit;
}

/// A live console session: a PTY master attached to a `/bin/sh` that joined the
/// replica's namespaces and cgroup.
pub struct RuntimeConsoleSession {
    pub session_id: Uuid,
    pub replica_index: u32,
    pub child_pid: u32,
    pub cgroup_path: PathBuf,
    pub pty: Box<dyn ConsolePty>,
    /// Drives child teardown + reaping. Always set by real runtimes; the fake
    /// runtime leaves it `None` (its console child is not a real process).
    pub reaper: Option<Box<dyn ConsoleReaper>>,
}

impl RuntimeConsoleSession {
    /// Terminate and reap the console child, returning how it ended. Idempotent
    /// in practice: the reaper handles an already-exited child. When no reaper
    /// is present (fake runtime) the reason is `Unknown`.
    pub async fn close(&mut self) -> ConsoleExit {
        match self.reaper.take() {
            Some(reaper) => reaper.reap().await,
            None => ConsoleExit::Unknown,
        }
    }
}

impl std::fmt::Debug for RuntimeConsoleSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConsoleSession")
            .field("session_id", &self.session_id)
            .field("replica_index", &self.replica_index)
            .field("child_pid", &self.child_pid)
            .field("cgroup_path", &self.cgroup_path)
            .field("pty", &"<console pty>")
            .field("reaper", &self.reaper.as_ref().map(|_| "<console reaper>"))
            .finish()
    }
}

/// A bidirectional console transport with terminal resize support. The blanket
/// supertraits let the websocket bridge use `AsyncReadExt`/`AsyncWriteExt`
/// directly on a `Box<dyn ConsolePty>`. Concrete implementations are provided
/// for the real PTY master and the fake runtime's in-memory pipe.
pub trait ConsolePty: AsyncRead + AsyncWrite + Unpin + Send {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
}

impl ConsolePty for crate::syscall::pty::PtyMaster {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        crate::syscall::pty::PtyMaster::resize(self, cols, rows)
    }
}
