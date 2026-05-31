use std::path::{Component, Path};

use crate::domain::RuntimeStartRequest;
use crate::runtime::error::RuntimeError;
use crate::runtime::plan::LinuxRuntimeProcessSpec;

pub(crate) fn validate_service_name(service_name: &str) -> Result<(), RuntimeError> {
    let valid = !service_name.is_empty()
        && service_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidServiceName {
            name: service_name.to_string(),
        })
    }
}

pub(crate) fn validate_process_spec(
    process: &LinuxRuntimeProcessSpec,
    manifest_path: &Path,
) -> Result<(), RuntimeError> {
    if process.argv.is_empty() {
        return Err(RuntimeError::EmptyArgv {
            path: manifest_path.to_path_buf(),
        });
    }
    if !process.argv[0].starts_with('/') {
        return Err(RuntimeError::InvalidArgv {
            argv0: process.argv[0].clone(),
        });
    }
    if !is_safe_absolute_workdir(&process.workdir) {
        return Err(RuntimeError::InvalidWorkdir {
            workdir: process.workdir.clone(),
        });
    }
    for (key, _) in &process.env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(RuntimeError::InvalidEnvironmentKey { key: key.clone() });
        }
    }
    Ok(())
}

fn is_safe_absolute_workdir(workdir: &str) -> bool {
    if workdir.as_bytes().contains(&0) {
        return false;
    }
    if workdir
        .split('/')
        .skip(1)
        .any(|component| matches!(component, "." | ".."))
    {
        return false;
    }
    let mut components = Path::new(workdir).components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return false;
    }
    components.all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn validate_resource_limits(request: &RuntimeStartRequest) -> Result<(), RuntimeError> {
    if request.cpu_millis == 0 {
        return Err(RuntimeError::InvalidResourceLimit {
            reason: "cpu_millis must be greater than zero".to_string(),
        });
    }
    if request.memory_bytes == 0 {
        return Err(RuntimeError::InvalidResourceLimit {
            reason: "memory_bytes must be greater than zero".to_string(),
        });
    }
    if let Some(pids) = request.pids_max
        && pids == 0
    {
        return Err(RuntimeError::InvalidResourceLimit {
            reason: "pids_max must be greater than zero".to_string(),
        });
    }
    if let Some(weight) = request.io_weight
        && (weight == 0 || weight > 10000)
    {
        return Err(RuntimeError::InvalidResourceLimit {
            reason: "io_weight must be between 1 and 10000".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_with_workdir(workdir: &str) -> LinuxRuntimeProcessSpec {
        LinuxRuntimeProcessSpec {
            argv: vec!["/bin/sh".to_string()],
            env: Vec::new(),
            workdir: workdir.to_string(),
        }
    }

    #[test]
    fn validate_process_spec_rejects_traversing_workdirs() {
        for workdir in ["/../sock/pwn", "/app/../sock", "/./app"] {
            let err = validate_process_spec(
                &process_with_workdir(workdir),
                Path::new("/tmp/manifest.json"),
            )
            .expect_err("traversing workdir must be rejected");
            assert!(
                matches!(err, RuntimeError::InvalidWorkdir { .. }),
                "unexpected error for {workdir}: {err:?}"
            );
        }
    }

    #[test]
    fn validate_process_spec_allows_absolute_normal_workdirs() {
        for workdir in ["/", "/srv/app"] {
            validate_process_spec(
                &process_with_workdir(workdir),
                Path::new("/tmp/manifest.json"),
            )
            .expect("normal absolute workdir");
        }
    }
}
