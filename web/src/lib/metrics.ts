import type { CpuCounters } from '../effect/schema'

export function cpuBusyDelta(prev: CpuCounters, curr: CpuCounters): {
  busyDelta: number
  totalDelta: number
} {
  const prevTotal =
    prev.user_jiffies +
    prev.nice_jiffies +
    prev.system_jiffies +
    prev.idle_jiffies +
    prev.iowait_jiffies
  const currTotal =
    curr.user_jiffies +
    curr.nice_jiffies +
    curr.system_jiffies +
    curr.idle_jiffies +
    curr.iowait_jiffies
  const prevBusy =
    prevTotal - prev.idle_jiffies - prev.iowait_jiffies
  const currBusy =
    currTotal - curr.idle_jiffies - curr.iowait_jiffies
  return {
    busyDelta: Math.max(0, currBusy - prevBusy),
    totalDelta: Math.max(0, currTotal - prevTotal),
  }
}

export function cpuPercent(prev: CpuCounters, curr: CpuCounters): number {
  const { busyDelta, totalDelta } = cpuBusyDelta(prev, curr)
  if (totalDelta === 0) return 0
  return Math.min(100, (busyDelta / totalDelta) * 100)
}

export function formatBytes(value: number): string {
  if (!Number.isFinite(value)) return '—'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let v = value
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i += 1
  }
  return `${v.toFixed(v < 10 ? 2 : 1)} ${units[i]}`
}
