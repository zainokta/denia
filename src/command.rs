use std::{
    collections::VecDeque,
    io,
    process::ExitStatus,
    sync::{Arc, Mutex},
};

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
    #[error("command failed with status {status}: {stderr}")]
    Failed { status: i32, stderr: String },
    #[error("fake command runner has no output queued")]
    NoFakeOutput,
    #[error("fake command runner state is poisoned")]
    Poisoned,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError>;

    /// Like [`CommandRunner::run`], but with extra environment variables set on
    /// the child process. The default ignores `envs` and delegates to `run`, so
    /// fakes and env-agnostic runners need no change; real runners override it.
    async fn run_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
    ) -> Result<CommandOutput, CommandError> {
        let _ = envs;
        self.run(program, args).await
    }
}

#[async_trait]
impl<T> CommandRunner for Arc<T>
where
    T: CommandRunner + ?Sized,
{
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        (**self).run(program, args).await
    }

    async fn run_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
    ) -> Result<CommandOutput, CommandError> {
        (**self).run_env(program, args, envs).await
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        self.run_env(program, args, &[]).await
    }

    async fn run_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
    ) -> Result<CommandOutput, CommandError> {
        let mut command = Command::new(program);
        command.args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().await?;

        let result = CommandOutput {
            status: status_code(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        if result.status == 0 {
            Ok(result)
        } else {
            Err(CommandError::Failed {
                status: result.status,
                stderr: result.stderr,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeCommandRunner {
    state: Arc<Mutex<FakeCommandRunnerState>>,
}

#[derive(Debug)]
struct FakeCommandRunnerState {
    commands: Vec<String>,
    outputs: VecDeque<CommandOutput>,
}

impl FakeCommandRunner {
    pub fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeCommandRunnerState {
                commands: Vec::new(),
                outputs: outputs.into(),
            })),
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
