// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'

describe('Job detail route', () => {
  it('route exports are defined', async () => {
    const mod = await import('./$jobId')
    expect(mod.Route).toBeDefined()
    expect(mod.JobDetail).toBeDefined()
  })

  it('components are functions', async () => {
    const mod = await import('./$jobId')
    expect(typeof mod.JobDetail).toBe('function')
  })
})
