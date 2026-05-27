use crate::domain::ResourceLimits;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCapacity {
    pub cpu_millis: u32,
    pub mem_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headroom {
    pub cpu_millis: u32,
    pub mem_bytes: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("insufficient capacity for reservation")]
    InsufficientCapacity,
}

#[derive(Debug, Clone)]
pub struct ResourceLedger {
    committed_cpu: u32,
    committed_mem: u64,
    allocatable_cpu: u32,
    allocatable_mem: u64,
}

impl ResourceLedger {
    pub fn new(host: HostCapacity, headroom: Headroom) -> Self {
        Self {
            committed_cpu: 0,
            committed_mem: 0,
            allocatable_cpu: host.cpu_millis.saturating_sub(headroom.cpu_millis),
            allocatable_mem: host.mem_bytes.saturating_sub(headroom.mem_bytes),
        }
    }

    /// Reserve a replica's budget (call before spawning, so concurrent scale-ups can't double-spend).
    pub fn try_reserve(&mut self, limits: &ResourceLimits) -> Result<(), LedgerError> {
        let next_cpu = self.committed_cpu.saturating_add(limits.cpu_millis);
        let next_mem = self.committed_mem.saturating_add(limits.memory_bytes);
        if next_cpu > self.allocatable_cpu || next_mem > self.allocatable_mem {
            return Err(LedgerError::InsufficientCapacity);
        }
        self.committed_cpu = next_cpu;
        self.committed_mem = next_mem;
        Ok(())
    }

    /// Free a replica's budget after it is killed.
    pub fn release(&mut self, limits: &ResourceLimits) {
        self.committed_cpu = self.committed_cpu.saturating_sub(limits.cpu_millis);
        self.committed_mem = self.committed_mem.saturating_sub(limits.memory_bytes);
    }

    pub fn committed_cpu(&self) -> u32 {
        self.committed_cpu
    }
    pub fn committed_mem(&self) -> u64 {
        self.committed_mem
    }
}

impl HostCapacity {
    /// Detect host totals from the running system. Memory from /proc/meminfo (reusing the
    /// node_metrics parser); CPU from the logical core count * 1000 millicores.
    pub fn detect() -> Self {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as u32;
        let mem_bytes = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| crate::observability::parse_meminfo(&s).ok())
            .map(|(total, _available)| total)
            .unwrap_or(0);
        Self {
            cpu_millis: cpu_count.saturating_mul(1000),
            mem_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ResourceLimits;

    #[test]
    fn ledger_denies_when_exceeding_capacity_minus_headroom() {
        // host 4000 millicores / 4 GiB ; headroom 1000 mc / 1 GiB => allocatable 3000 mc / 3 GiB
        let mut l = ResourceLedger::new(
            HostCapacity {
                cpu_millis: 4000,
                mem_bytes: 4 << 30,
            },
            Headroom {
                cpu_millis: 1000,
                mem_bytes: 1 << 30,
            },
        );
        let lim = ResourceLimits {
            cpu_millis: 1000,
            memory_bytes: 1 << 30,
        };
        assert!(l.try_reserve(&lim).is_ok()); // 1000 / 1 GiB
        assert!(l.try_reserve(&lim).is_ok()); // 2000 / 2 GiB
        assert!(l.try_reserve(&lim).is_ok()); // 3000 / 3 GiB == allocatable
        assert!(l.try_reserve(&lim).is_err()); // would exceed allocatable
        l.release(&lim);
        assert!(l.try_reserve(&lim).is_ok()); // freed, fits again
    }

    #[test]
    fn detect_returns_nonzero_capacity() {
        let cap = HostCapacity::detect();
        assert!(
            cap.cpu_millis >= 1000,
            "expected at least one core worth of millis"
        );
        assert!(cap.mem_bytes > 0, "expected nonzero total memory");
    }
}
