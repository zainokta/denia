use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("metric file was empty")]
    Empty,
    #[error("metric value was not an unsigned integer")]
    InvalidInteger,
    #[error("invalid metrics service name: {name}")]
    InvalidServiceName { name: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn parse_memory_current(input: &str) -> Result<u64, MetricsError> {
    let value = input.trim();
    if value.is_empty() {
        return Err(MetricsError::Empty);
    }
    value.parse().map_err(|_| MetricsError::InvalidInteger)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CpuStat {
    pub usage_usec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricSnapshot {
    pub service_name: String,
    pub cpu_usage_usec: u64,
    pub memory_current_bytes: u64,
}

pub fn parse_cpu_stat(input: &str) -> Result<CpuStat, MetricsError> {
    for line in input.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("usage_usec") {
            let value = parts.next().ok_or(MetricsError::InvalidInteger)?;
            return Ok(CpuStat {
                usage_usec: value.parse().map_err(|_| MetricsError::InvalidInteger)?,
            });
        }
    }
    Err(MetricsError::Empty)
}

#[derive(Debug, Clone)]
pub struct CgroupMetricsReader {
    cgroup_root: PathBuf,
}

impl CgroupMetricsReader {
    pub fn new(cgroup_root: impl Into<PathBuf>) -> Self {
        Self {
            cgroup_root: cgroup_root.into(),
        }
    }

    pub fn read_service(
        &self,
        service_name: &str,
        deployment_id: uuid::Uuid,
    ) -> Result<MetricSnapshot, MetricsError> {
        validate_service_name(service_name)?;
        let cgroup_path = self
            .cgroup_root
            .join(service_name)
            .join(deployment_id.to_string());
        read_snapshot(service_name, &cgroup_path)
    }
}

fn read_snapshot(service_name: &str, cgroup_path: &Path) -> Result<MetricSnapshot, MetricsError> {
    let cpu = parse_cpu_stat(&std::fs::read_to_string(cgroup_path.join("cpu.stat"))?)?;
    let memory_current_bytes = parse_memory_current(&std::fs::read_to_string(
        cgroup_path.join("memory.current"),
    )?)?;

    Ok(MetricSnapshot {
        service_name: service_name.to_string(),
        cpu_usage_usec: cpu.usage_usec,
        memory_current_bytes,
    })
}

fn validate_service_name(service_name: &str) -> Result<(), MetricsError> {
    let valid = !service_name.is_empty()
        && service_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(MetricsError::InvalidServiceName {
            name: service_name.to_string(),
        })
    }
}
