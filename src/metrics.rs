use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MetricsError {
    #[error("metric file was empty")]
    Empty,
    #[error("metric value was not an unsigned integer")]
    InvalidInteger,
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
