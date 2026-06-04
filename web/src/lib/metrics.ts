import type { CpuCounters } from '../effect/schema'

// Split a cgroup/procfs CPU jiffies counter into busy vs total. `iowait` counts
// as busy here (the CPU is occupied servicing I/O), matching the node-gauge
// derivation used on the dashboard and observability routes.
export function cpuBusyTotal(cpu: CpuCounters): {
  busy: number
  total: number
} {
  const total =
    cpu.user_jiffies +
    cpu.nice_jiffies +
    cpu.system_jiffies +
    cpu.idle_jiffies +
    cpu.iowait_jiffies
  const busy =
    cpu.user_jiffies + cpu.nice_jiffies + cpu.system_jiffies + cpu.iowait_jiffies
  return { busy, total }
}

// Instantaneous CPU% from the delta between two cumulative snapshots. The raw
// counters are monotonic since boot, so a single snapshot says nothing — the
// percentage only exists between two readings. Clamped to [0, 100].
export function cpuPercentDelta(
  prev: { busy: number; total: number },
  curr: { busy: number; total: number },
): number {
  const dBusy = curr.busy - prev.busy
  const dTotal = curr.total - prev.total
  if (dTotal <= 0) return 0
  return Math.max(0, Math.min(100, (dBusy / dTotal) * 100))
}

// Convenience: CPU% between two raw `CpuCounters` snapshots.
export function cpuPercent(prev: CpuCounters, curr: CpuCounters): number {
  return cpuPercentDelta(cpuBusyTotal(prev), cpuBusyTotal(curr))
}
