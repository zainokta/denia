use std::path::Path;

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
    if !process.workdir.starts_with('/') {
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
    Ok(())
}
