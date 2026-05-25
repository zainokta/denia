// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'

describe('Jobs list route', () => {
  it('route exports are defined', async () => {
    const mod = await import('./index')
    expect(mod.Route).toBeDefined()
    expect(mod.JobsIndex).toBeDefined()
  })

  it('components are functions', async () => {
    const mod = await import('./index')
    expect(typeof mod.JobsIndex).toBe('function')
  })
})
