import { describe, expect, it } from 'vitest'
import { cpuBusyTotal, cpuPercent, cpuPercentDelta } from './metrics'
import type { CpuCounters } from '../effect/schema'

const counters = (over: Partial<CpuCounters> = {}): CpuCounters => ({
  user_jiffies: 0,
  nice_jiffies: 0,
  system_jiffies: 0,
  idle_jiffies: 0,
  iowait_jiffies: 0,
  ...over,
})

describe('cpuBusyTotal', () => {
  it('counts user+nice+system+iowait as busy and includes idle in total', () => {
    const { busy, total } = cpuBusyTotal(
      counters({
        user_jiffies: 10,
        nice_jiffies: 1,
        system_jiffies: 4,
        idle_jiffies: 80,
        iowait_jiffies: 5,
      }),
    )
    expect(busy).toBe(20)
    expect(total).toBe(100)
  })
})

describe('cpuPercentDelta', () => {
  it('returns the busy share of the total delta', () => {
    const pct = cpuPercentDelta({ busy: 0, total: 0 }, { busy: 50, total: 200 })
    expect(pct).toBe(25)
  })

  it('returns 0 when the total delta is non-positive (no elapsed time)', () => {
    expect(cpuPercentDelta({ busy: 10, total: 100 }, { busy: 10, total: 100 })).toBe(0)
    expect(cpuPercentDelta({ busy: 0, total: 100 }, { busy: 0, total: 50 })).toBe(0)
  })

  it('clamps into [0, 100]', () => {
    expect(cpuPercentDelta({ busy: 0, total: 0 }, { busy: 300, total: 100 })).toBe(100)
    expect(cpuPercentDelta({ busy: 50, total: 0 }, { busy: 0, total: 100 })).toBe(0)
  })
})

describe('cpuPercent', () => {
  it('derives a percentage from two raw counter snapshots', () => {
    const prev = counters({ idle_jiffies: 100 })
    const curr = counters({ user_jiffies: 25, idle_jiffies: 175 })
    // busy delta 25, total delta 100 => 25%
    expect(cpuPercent(prev, curr)).toBe(25)
  })
})
