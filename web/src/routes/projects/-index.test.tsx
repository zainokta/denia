// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'

describe('Projects list route', () => {
  it('route exports are defined', async () => {
    const mod = await import('./index')
    expect(mod.Route).toBeDefined()
    expect(mod.ProjectsIndex).toBeDefined()
  })

  it('components are functions', async () => {
    const mod = await import('./index')
    expect(typeof mod.ProjectsIndex).toBe('function')
  })
})
