// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import { DeployPhase } from './DeployPhase'

describe('DeployPhase', () => {
  it('shows acquiring active and earlier done for Building status', () => {
    const { container } = render(<DeployPhase status="Building" />)

    const steps = container.querySelectorAll('.kicker')
    expect(steps.length).toBe(4)
    expect(steps[1].textContent).toBe('acquiring')

    const warn = container.querySelector('.signal-warn')
    expect(warn).toBeTruthy()
    const signals = container.querySelectorAll('.signal')
    expect(signals.length).toBe(2)
  })

  it('shows all steps done with ok for Healthy', () => {
    const { container } = render(<DeployPhase status="Healthy" />)

    const oks = container.querySelectorAll('.signal-ok')
    expect(oks.length).toBe(4)
  })

  it('shows fault signal on live step for Failed', () => {
    const { container } = render(<DeployPhase status="Failed" />)

    const fault = container.querySelector('.signal-fault')
    expect(fault).toBeTruthy()
  })

  it('returns null for unknown status', () => {
    const { container } = render(<DeployPhase status="Unknown" />)
    expect(container.innerHTML).toBe('')
  })
})
