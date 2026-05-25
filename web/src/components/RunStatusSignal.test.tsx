// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { render, screen } from '@testing-library/react'
import { RunStatusSignal } from './RunStatusSignal'

describe('RunStatusSignal', () => {
  it('Succeeded maps to signal-ok', () => {
    const { container } = render(<RunStatusSignal status="Succeeded" />)
    const dot = container.querySelector('.signal-ok')
    expect(dot).toBeTruthy()
    expect(screen.getByText('Succeeded')).toBeTruthy()
  })

  it('Failed maps to signal-fault', () => {
    const { container } = render(<RunStatusSignal status="Failed" />)
    const dot = container.querySelector('.signal-fault')
    expect(dot).toBeTruthy()
  })

  it('Running maps to signal-warn', () => {
    const { container } = render(<RunStatusSignal status="Running" />)
    const dot = container.querySelector('.signal-warn')
    expect(dot).toBeTruthy()
  })

  it('Pending maps to signal-warn', () => {
    const { container } = render(<RunStatusSignal status="Pending" />)
    const dot = container.querySelector('.signal-warn')
    expect(dot).toBeTruthy()
  })

  it('Skipped has no signal class', () => {
    const { container } = render(<RunStatusSignal status="Skipped" />)
    const dot = container.querySelector('.signal')
    expect(dot?.className).not.toContain('signal-warn')
    expect(dot?.className).not.toContain('signal-ok')
    expect(dot?.className).not.toContain('signal-fault')
    expect(dot?.className).not.toContain('signal-steady')
  })
})
