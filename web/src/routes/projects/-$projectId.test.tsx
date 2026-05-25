// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'

describe('Project detail route', () => {
  it('route exports are defined', async () => {
    const mod = await import('./$projectId')
    expect(mod.Route).toBeDefined()
    expect(mod.ProjectDetail).toBeDefined()
  })

  it('components are functions', async () => {
    const mod = await import('./$projectId')
    expect(typeof mod.ProjectDetail).toBe('function')
  })
})
