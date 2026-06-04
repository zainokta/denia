import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  getActiveProject,
  setActiveProject,
  subscribe,
} from './active-project-store'

afterEach(() => {
  // Reset to the default (empty = all projects) between tests.
  setActiveProject('')
})

describe('active-project-store', () => {
  it('defaults to empty (all projects)', () => {
    expect(getActiveProject()).toBe('')
  })

  it('setActiveProject stores and getActiveProject retrieves it', () => {
    setActiveProject('proj-1')
    expect(getActiveProject()).toBe('proj-1')
  })

  it('setting an empty id clears the active project', () => {
    setActiveProject('proj-1')
    setActiveProject('')
    expect(getActiveProject()).toBe('')
  })

  it('notifies subscribers on change and stops after unsubscribe', () => {
    const listener = vi.fn()
    const unsub = subscribe(listener)
    setActiveProject('proj-2')
    expect(listener).toHaveBeenCalledTimes(1)
    setActiveProject('')
    expect(listener).toHaveBeenCalledTimes(2)
    unsub()
    setActiveProject('proj-3')
    expect(listener).toHaveBeenCalledTimes(2)
  })
})
