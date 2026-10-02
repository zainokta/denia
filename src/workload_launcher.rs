use std::{
    ffi::OsString,
    os::unix::process::CommandExt,
    process::{Command, ExitStatus, Stdio},
};

use thiserror::Error;

use crate::syscall::{self, caps};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadLauncherConfig {
    pub uid: u32,
    pub gid: u32,
    pub child_argv: Vec<OsString>,
}

#[derive(Debug, Error)]
pub enum WorkloadLauncherError {
    #[error("invalid workload launcher argument: {value}")]
    InvalidArgument { value: String },
    #[error("workload launcher child argv is empty")]
    EmptyChildArgv,
    #[error("workload launcher child terminated by signal")]
    ChildSignaled,
    #[error("workload launcher hardening failed: {0}")]
    Hardening(#[from] syscall::SyscallError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn parse_args<I>(args: I) -> Result<WorkloadLauncherConfig, WorkloadLauncherError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut uid = 0;
    let mut gid = 0;
    loop {
        match args.next() {
            Some(arg) if arg == "--" => break,
            Some(arg) if arg == "--uid" => {
                let value = args.next().ok_or(WorkloadLauncherError::InvalidArgument {
                    value: "--uid".to_string(),
                })?;
                uid = value.to_string_lossy().parse::<u32>().map_err(|_| {
                    WorkloadLauncherError::InvalidArgument {
                        value: value.to_string_lossy().to_string(),
                    }
                })?;
            }
            Some(arg) if arg == "--gid" => {
                let value = args.next().ok_or(WorkloadLauncherError::InvalidArgument {
                    value: "--gid".to_string(),
                })?;
                gid = value.to_string_lossy().parse::<u32>().map_err(|_| {
                    WorkloadLauncherError::InvalidArgument {
                        value: value.to_string_lossy().to_string(),
                    }
                })?;
            }
            Some(arg) => {
                return Err(WorkloadLauncherError::InvalidArgument {
                    value: arg.to_string_lossy().to_string(),
                });
            }
            None => return Err(WorkloadLauncherError::EmptyChildArgv),
        }
    }

    let child_argv = args.collect::<Vec<_>>();
    if child_argv.is_empty() {
        return Err(WorkloadLauncherError::EmptyChildArgv);
    }

    Ok(WorkloadLauncherConfig {
        uid,
        gid,
        child_argv,
    })
}

pub fn run_from_args<I>(args: I) -> Result<i32, WorkloadLauncherError>
where
    I: IntoIterator<Item = OsString>,
{
    run(parse_args(args)?)
}

pub fn run(config: WorkloadLauncherConfig) -> Result<i32, WorkloadLauncherError> {
    caps::set_no_new_privs()?;
    let mut command = Command::new(&config.child_argv[0]);
    command
        .args(&config.child_argv[1..])
        .stdin(Stdio::null())
        .gid(config.gid)
        .uid(config.uid);
    let status = command.status()?;
    caps::drop_bounding_caps()?;
    exit_code(status)
}

fn exit_code(status: ExitStatus) -> Result<i32, WorkloadLauncherError> {
    status.code().ok_or(WorkloadLauncherError::ChildSignaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_reads_child_contract() {
        let config = parse_args([
            OsString::from("--uid"),
            OsString::from("101"),
            OsString::from("--gid"),
            OsString::from("101"),
            OsString::from("--"),
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from("true"),
        ])
        .expect("config");

        assert_eq!(
            config.child_argv,
            vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("true")
            ]
        );
        assert_eq!(config.uid, 101);
        assert_eq!(config.gid, 101);
    }

    #[test]
    fn parse_args_requires_separator() {
        let error = parse_args([OsString::from("/bin/sh")]).expect_err("separator");

        assert!(matches!(
            error,
            WorkloadLauncherError::InvalidArgument { .. }
        ));
    }

    #[test]
    fn parse_args_requires_child_argv() {
        let error = parse_args([OsString::from("--")]).expect_err("child argv");

        assert!(matches!(error, WorkloadLauncherError::EmptyChildArgv));
    }
}
