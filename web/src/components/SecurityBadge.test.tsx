// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import { SecurityBadge } from './SecurityBadge'

describe('SecurityBadge', () => {
  it('shows sandboxed with signal-steady for full posture', () => {
    const { container } = render(
      <SecurityBadge
        security={{
          userns: true,
          mapped_uid: 100000,
          no_new_privs: true,
          caps_dropped: true,
        }}
      />,
    )

    expect(container.querySelector('.signal-steady')).toBeTruthy()
    expect(container.textContent).toMatch(/sandboxed/)
    expect(container.querySelector('.signal-fault')).toBeFalsy()
  })

  it('shows signal-fault for weak posture with gap name', () => {
    const { container } = render(
      <SecurityBadge
        security={{
          userns: true,
          mapped_uid: null,
          no_new_privs: true,
          caps_dropped: false,
        }}
      />,
    )

    expect(container.querySelector('.signal-fault')).toBeTruthy()
    expect(container.textContent).toMatch(/weak/)
    expect(container.textContent).toContain('caps')
  })

  it('shows muted n/a when security is undefined', () => {
    const { container } = render(<SecurityBadge />)

    expect(container.querySelector('.signal-steady')).toBeFalsy()
    expect(container.querySelector('.signal-fault')).toBeFalsy()
    expect(container.textContent).toMatch(/posture: n\/a/)
  })
})
