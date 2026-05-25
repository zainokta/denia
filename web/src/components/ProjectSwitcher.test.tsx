// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'

describe('ProjectSwitcher', () => {
  it('component exports are defined', async () => {
    const mod = await import('./ProjectSwitcher')
    expect(mod.ProjectSwitcher).toBeDefined()
    expect(typeof mod.ProjectSwitcher).toBe('function')
  })
})
