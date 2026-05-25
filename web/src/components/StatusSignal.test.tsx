// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { render, screen } from '@testing-library/react'
import { StatusSignal } from './StatusSignal'

describe('StatusSignal', () => {
  it('Healthy maps to signal-ok', () => {
    const { container } = render(<StatusSignal status="Healthy" />)
    expect(screen.getByText('Healthy')).toBeTruthy()
    expect(container.querySelector('.signal-ok')).toBeTruthy()
  })

  it('Failed maps to signal-fault', () => {
    const { container } = render(<StatusSignal status="Failed" />)
    expect(container.querySelector('.signal-fault')).toBeTruthy()
  })

  it('Building maps to signal-warn', () => {
    const { container } = render(<StatusSignal status="Building" />)
    expect(container.querySelector('.signal-warn')).toBeTruthy()
  })

  it('Stopped is muted (no signal class)', () => {
    const { container } = render(<StatusSignal status="Stopped" />)
    expect(container.querySelector('.signal-ok')).toBeFalsy()
    expect(container.querySelector('.signal-fault')).toBeFalsy()
    expect(container.querySelector('.signal-warn')).toBeFalsy()
  })

  it('Starting maps to signal-warn', () => {
    const { container } = render(<StatusSignal status="Starting" />)
    expect(container.querySelector('.signal-warn')).toBeTruthy()
  })

  it('Pending maps to signal-warn', () => {
    const { container } = render(<StatusSignal status="Pending" />)
    expect(container.querySelector('.signal-warn')).toBeTruthy()
  })
})
