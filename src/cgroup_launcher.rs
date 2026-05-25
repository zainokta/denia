use std::{ffi::OsString, os::unix::process::CommandExt, path::PathBuf, process::Command};

use thiserror::Error;

pub const MODE_ARG: &str = "__denia_cgroup_launcher";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupLauncherConfig {
    pub cgroup_procs: PathBuf,
    pub ready_file: PathBuf,
    pub child_argv: Vec<OsString>,
}

#[derive(Debug, Error)]
pub enum CgroupLauncherError {
    #[error("missing cgroup launcher argument: {name}")]
    MissingArgument { name: &'static str },
    #[error("invalid cgroup launcher argument: {value}")]
    InvalidArgument { value: String },
    #[error("cgroup launcher child argv is empty")]
    EmptyChildArgv,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn parse_args<I>(args: I) -> Result<CgroupLauncherConfig, CgroupLauncherError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut cgroup_procs = None;
    let mut ready_file = None;
    let mut child_argv = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "--" {
            child_argv.extend(args);
            break;
        }
        if arg == "--cgroup-procs" {
            cgroup_procs = Some(
                args.next()
                    .ok_or(CgroupLauncherError::MissingArgument {
                        name: "--cgroup-procs",
                    })?
                    .into(),
            );
            continue;
        }
        if arg == "--ready-file" {
            ready_file = Some(
                args.next()
                    .ok_or(CgroupLauncherError::MissingArgument {
                        name: "--ready-file",
                    })?
                    .into(),
            );
            continue;
        }
        return Err(CgroupLauncherError::InvalidArgument {
            value: arg.to_string_lossy().to_string(),
        });
    }

    let cgroup_procs = cgroup_procs.ok_or(CgroupLauncherError::MissingArgument {
        name: "--cgroup-procs",
    })?;
    let ready_file = ready_file.ok_or(CgroupLauncherError::MissingArgument {
        name: "--ready-file",
    })?;
    if child_argv.is_empty() {
        return Err(CgroupLauncherError::EmptyChildArgv);
    }

    Ok(CgroupLauncherConfig {
        cgroup_procs,
        ready_file,
        child_argv,
    })
}

pub fn run_from_args<I>(args: I) -> Result<(), CgroupLauncherError>
where
    I: IntoIterator<Item = OsString>,
{
    run(parse_args(args)?)
}

pub fn run(config: CgroupLauncherConfig) -> Result<(), CgroupLauncherError> {
    std::fs::write(&config.cgroup_procs, format!("{}\n", std::process::id()))?;
    std::fs::write(&config.ready_file, b"ready\n")?;
    let error = Command::new(&config.child_argv[0])
        .args(&config.child_argv[1..])
        .exec();
    Err(CgroupLauncherError::Io(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_reads_cgroup_procs_and_child_argv() {
        let config = parse_args([
            OsString::from("--cgroup-procs"),
            OsString::from("/sys/fs/cgroup/denia/web/cgroup.procs"),
            OsString::from("--ready-file"),
            OsString::from("/var/lib/denia/runtime/web/launch.ready"),
            OsString::from("--"),
            OsString::from("unshare"),
            OsString::from("--fork"),
        ])
        .expect("config");

        assert_eq!(
            config.cgroup_procs,
            PathBuf::from("/sys/fs/cgroup/denia/web/cgroup.procs")
        );
        assert_eq!(
            config.ready_file,
            PathBuf::from("/var/lib/denia/runtime/web/launch.ready")
        );
        assert_eq!(
            config.child_argv,
            vec![OsString::from("unshare"), OsString::from("--fork")]
        );
    }

    #[test]
    fn parse_args_requires_child_argv() {
        let error = parse_args([
            OsString::from("--cgroup-procs"),
            OsString::from("/sys/fs/cgroup/denia/web/cgroup.procs"),
            OsString::from("--ready-file"),
            OsString::from("/var/lib/denia/runtime/web/launch.ready"),
            OsString::from("--"),
        ])
        .expect_err("child argv");

        assert!(matches!(error, CgroupLauncherError::EmptyChildArgv));
    }
}
