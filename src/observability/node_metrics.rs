use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeMetricsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error in {field}: {reason}")]
    Parse { field: &'static str, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuCounters {
    pub user_jiffies: u64,
    pub nice_jiffies: u64,
    pub system_jiffies: u64,
    pub idle_jiffies: u64,
    pub iowait_jiffies: u64,
}

impl CpuCounters {
    pub fn total_jiffies(&self) -> u64 {
        self.user_jiffies
            .saturating_add(self.nice_jiffies)
            .saturating_add(self.system_jiffies)
            .saturating_add(self.idle_jiffies)
            .saturating_add(self.iowait_jiffies)
    }

    pub fn busy_jiffies(&self) -> u64 {
        self.total_jiffies()
            .saturating_sub(self.idle_jiffies)
            .saturating_sub(self.iowait_jiffies)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub cpu: CpuCounters,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    pub disk_total_bytes: u64,
    pub disk_available_bytes: u64,
    pub recorded_at: String,
}

pub fn parse_proc_stat_cpu(input: &str) -> Result<CpuCounters, NodeMetricsError> {
    let line = input
        .lines()
        .find(|l| l.starts_with("cpu ") || l.starts_with("cpu\t"))
        .ok_or(NodeMetricsError::Parse {
            field: "cpu",
            reason: "no cpu aggregate line".to_string(),
        })?;
    let mut parts = line.split_whitespace();
    let _label = parts.next();
    let values: Vec<u64> = parts
        .take(5)
        .map(|v| {
            v.parse::<u64>().map_err(|_| NodeMetricsError::Parse {
                field: "cpu",
                reason: format!("non-integer '{v}'"),
            })
        })
        .collect::<Result<_, _>>()?;
    if values.len() < 5 {
        return Err(NodeMetricsError::Parse {
            field: "cpu",
            reason: "fewer than 5 fields".to_string(),
        });
    }
    Ok(CpuCounters {
        user_jiffies: values[0],
        nice_jiffies: values[1],
        system_jiffies: values[2],
        idle_jiffies: values[3],
        iowait_jiffies: values[4],
    })
}

pub fn parse_meminfo(input: &str) -> Result<(u64, u64), NodeMetricsError> {
    let mut total = None;
    let mut available = None;
    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = Some(parse_kb_value(rest, "MemTotal")?);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = Some(parse_kb_value(rest, "MemAvailable")?);
        }
    }
    let total = total.ok_or(NodeMetricsError::Parse {
        field: "MemTotal",
        reason: "not found".to_string(),
    })?;
    let available = available.ok_or(NodeMetricsError::Parse {
        field: "MemAvailable",
        reason: "not found".to_string(),
    })?;
    Ok((total, available))
}

fn parse_kb_value(input: &str, field: &'static str) -> Result<u64, NodeMetricsError> {
    let value = input
        .trim()
        .strip_suffix("kB")
        .unwrap_or(input)
        .trim()
        .parse::<u64>()
        .map_err(|_| NodeMetricsError::Parse {
            field,
            reason: format!("non-integer '{input}'"),
        })?;
    Ok(value.saturating_mul(1024))
}

pub fn parse_loadavg(input: &str) -> Result<(f64, f64, f64), NodeMetricsError> {
    let mut parts = input.split_whitespace();
    let one = parts
        .next()
        .ok_or(NodeMetricsError::Parse {
            field: "load 1m",
            reason: "missing".to_string(),
        })?
        .parse::<f64>()
        .map_err(|e| NodeMetricsError::Parse {
            field: "load 1m",
            reason: e.to_string(),
        })?;
    let five = parts
        .next()
        .ok_or(NodeMetricsError::Parse {
            field: "load 5m",
            reason: "missing".to_string(),
        })?
        .parse::<f64>()
        .map_err(|e| NodeMetricsError::Parse {
            field: "load 5m",
            reason: e.to_string(),
        })?;
    let fifteen = parts
        .next()
        .ok_or(NodeMetricsError::Parse {
            field: "load 15m",
            reason: "missing".to_string(),
        })?
        .parse::<f64>()
        .map_err(|e| NodeMetricsError::Parse {
            field: "load 15m",
            reason: e.to_string(),
        })?;
    Ok((one, five, fifteen))
}

#[derive(Debug, Clone)]
pub struct NodeMetricsReader {
    proc_dir: PathBuf,
    disk_path: PathBuf,
}

impl NodeMetricsReader {
    pub fn new(disk_path: impl Into<PathBuf>) -> Self {
        Self::with_proc("/proc", disk_path)
    }

    pub fn with_proc(proc_dir: impl Into<PathBuf>, disk_path: impl Into<PathBuf>) -> Self {
        Self {
            proc_dir: proc_dir.into(),
            disk_path: disk_path.into(),
        }
    }

    pub fn read(&self) -> Result<NodeSnapshot, NodeMetricsError> {
        let cpu = parse_proc_stat_cpu(&std::fs::read_to_string(self.proc_dir.join("stat"))?)?;
        let (memory_total_bytes, memory_available_bytes) =
            parse_meminfo(&std::fs::read_to_string(self.proc_dir.join("meminfo"))?)?;
        let (load_1m, load_5m, load_15m) =
            parse_loadavg(&std::fs::read_to_string(self.proc_dir.join("loadavg"))?)?;
        let (disk_total_bytes, disk_available_bytes) = read_disk(&self.disk_path)?;
        Ok(NodeSnapshot {
            cpu,
            memory_total_bytes,
            memory_available_bytes,
            load_1m,
            load_5m,
            load_15m,
            disk_total_bytes,
            disk_available_bytes,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

fn read_disk(path: &Path) -> Result<(u64, u64), NodeMetricsError> {
    let stat = rustix::fs::statvfs(path).map_err(|e| NodeMetricsError::Io(e.into()))?;
    let block_size = stat.f_frsize as u64;
    let total = (stat.f_blocks as u64).saturating_mul(block_size);
    let available = (stat.f_bavail as u64).saturating_mul(block_size);
    Ok((total, available))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_aggregate() {
        let input = "cpu  100 20 30 1000 5 0 0 0 0 0\ncpu0 50 10 20 500 2\n";
        let cpu = parse_proc_stat_cpu(input).expect("cpu");
        assert_eq!(cpu.user_jiffies, 100);
        assert_eq!(cpu.idle_jiffies, 1000);
        assert_eq!(cpu.iowait_jiffies, 5);
        assert_eq!(cpu.busy_jiffies(), 100 + 20 + 30);
    }

    #[test]
    fn parses_meminfo_in_bytes() {
        let input = "MemTotal:        16384000 kB\nMemFree:          1024 kB\nMemAvailable:    8000000 kB\n";
        let (total, available) = parse_meminfo(input).expect("meminfo");
        assert_eq!(total, 16384000u64 * 1024);
        assert_eq!(available, 8000000u64 * 1024);
    }

    #[test]
    fn parses_loadavg_three_values() {
        let (a, b, c) = parse_loadavg("0.50 1.25 2.75 1/200 1234\n").expect("loadavg");
        assert!((a - 0.5).abs() < f64::EPSILON);
        assert!((b - 1.25).abs() < f64::EPSILON);
        assert!((c - 2.75).abs() < f64::EPSILON);
    }
}
