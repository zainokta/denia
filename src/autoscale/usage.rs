use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;

use crate::autoscale::registry::{Replica, ReplicaState};
use crate::domain::ResourceLimits;
use crate::observability::metrics::CgroupMetricsReader;

/// CPU percent of one replica's limit, from a cumulative-usec delta over a window.
pub fn cpu_pct(prev_usec: u64, cur_usec: u64, window_us: u64, cpu_millis: u32) -> u32 {
    if window_us == 0 || cpu_millis == 0 {
        return 0;
    }
    let delta = cur_usec.saturating_sub(prev_usec);
    // pct = delta / (window_us * cpu_millis/1000) * 100  ==  delta * 100_000 / (window_us * cpu_millis)
    let denom = window_us.saturating_mul(cpu_millis as u64);
    if denom == 0 {
        return 0;
    }
    ((delta.saturating_mul(100_000)) / denom) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServiceUsage {
    pub avg_cpu_pct: u32,
    pub avg_mem_pct: u32,
    pub max_mem_pct: u32,
    pub replica_count: u32,
}

impl ServiceUsage {
    /// Aggregate per-replica (cpu_pct, mem_pct) pairs.
    pub fn aggregate(per_replica: &[(u32, u32)]) -> Self {
        let n = per_replica.len() as u32;
        if n == 0 {
            return Self::default();
        }
        let sum_cpu: u64 = per_replica.iter().map(|(c, _)| *c as u64).sum();
        let sum_mem: u64 = per_replica.iter().map(|(_, m)| *m as u64).sum();
        let max_mem = per_replica.iter().map(|(_, m)| *m).max().unwrap_or(0);
        Self {
            avg_cpu_pct: (sum_cpu / n as u64) as u32,
            avg_mem_pct: (sum_mem / n as u64) as u32,
            max_mem_pct: max_mem,
            replica_count: n,
        }
    }
}

/// Holds previous cumulative CPU usec + sample instant per replica id, to compute deltas.
#[derive(Default)]
pub struct UsageSampler {
    prev: HashMap<Uuid, (u64, Instant)>,
}

impl UsageSampler {
    /// Sample each Healthy replica's cgroup, compute per-replica cpu%/mem%, aggregate.
    /// `limits` is the per-replica ResourceLimits. Replicas without a prior sample contribute 0% CPU this tick.
    pub fn sample(
        &mut self,
        service_name: &str,
        replicas: &[Replica],
        reader: &CgroupMetricsReader,
        limits: &ResourceLimits,
    ) -> ServiceUsage {
        let now = Instant::now();
        let mut pairs = Vec::new();
        for r in replicas.iter().filter(|r| r.state == ReplicaState::Healthy) {
            let snap =
                match reader.read_replica(service_name, r.service_id, r.deployment_id, r.index) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
            let cpu = match self.prev.get(&r.id) {
                Some((prev_usec, prev_t)) => {
                    let window_us = now.duration_since(*prev_t).as_micros() as u64;
                    cpu_pct(
                        *prev_usec,
                        snap.cpu_usage_usec,
                        window_us,
                        limits.cpu_millis,
                    )
                }
                None => 0,
            };
            self.prev.insert(r.id, (snap.cpu_usage_usec, now));
            let mem = if limits.memory_bytes == 0 {
                0
            } else {
                ((snap.memory_current_bytes.saturating_mul(100)) / limits.memory_bytes) as u32
            };
            pairs.push((cpu, mem));
        }
        ServiceUsage::aggregate(&pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_pct_from_delta() {
        // 800ms CPU over a 1s window at a 1000m (1 core) limit => 80%
        assert_eq!(cpu_pct(0, 800_000, 1_000_000, 1000), 80);
        // same usage at a 500m (half core) limit => 160%
        assert_eq!(cpu_pct(0, 800_000, 1_000_000, 500), 160);
        // no time elapsed => 0 (avoid div-by-zero)
        assert_eq!(cpu_pct(0, 800_000, 0, 1000), 0);
    }

    #[test]
    fn usage_aggregates_avg_cpu_and_mem() {
        // A: 80% cpu, 50% mem ; B: 40% cpu, 75% mem => avg_cpu 60, avg_mem 62, max_mem 75
        let u = ServiceUsage::aggregate(&[(80, 50), (40, 75)]);
        assert_eq!(u.avg_cpu_pct, 60);
        assert_eq!(u.avg_mem_pct, 62);
        assert_eq!(u.max_mem_pct, 75);
        assert_eq!(u.replica_count, 2);
    }

    #[test]
    fn usage_empty_is_zero() {
        let u = ServiceUsage::aggregate(&[]);
        assert_eq!(
            (u.avg_cpu_pct, u.avg_mem_pct, u.max_mem_pct, u.replica_count),
            (0, 0, 0, 0)
        );
    }
}
