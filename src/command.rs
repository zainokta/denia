use std::{collections::VecDeque, io, process::ExitStatus, sync::Mutex};

use async_trait::async_trait;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("failed to run command: {0}")]
    Io(#[from] io::Error),
    #[error("fake command runner has no output queued")]
    NoFakeOutput,
    #[error("fake command runner state is poisoned")]
    Poisoned,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        let output = Command::new(program).args(args).output().await?;

        Ok(CommandOutput {
            status: status_code(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug)]
pub struct FakeCommandRunner {
    state: Mutex<FakeCommandRunnerState>,
}

#[derive(Debug)]
struct FakeCommandRunnerState {
    commands: Vec<String>,
    outputs: VecDeque<CommandOutput>,
}

impl FakeCommandRunner {
    pub fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            state: Mutex::new(FakeCommandRunnerState {
                commands: Vec::new(),
                outputs: outputs.into(),
            }),
        }
    }

    pub async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        <Self as CommandRunner>::run(self, program, args).await
    }

    pub fn commands(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fake command runner state lock")
            .commands
            .clone()
    }
}

#[async_trait]
impl CommandRunner for FakeCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        let mut state = self.state.lock().map_err(|_| CommandError::Poisoned)?;
        state.commands.push(format_command(program, args));
        state.outputs.pop_front().ok_or(CommandError::NoFakeOutput)
    }
}

fn format_command(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(unix)]
fn status_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

#[cfg(not(unix))]
fn status_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}
