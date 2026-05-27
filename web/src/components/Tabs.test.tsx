// @vitest-environment jsdom
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import { vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { Tabs } from './Tabs'

const TABS = [
  { id: 'overview', label: 'Overview' },
  { id: 'config', label: 'Config' },
  { id: 'logs', label: 'Logs' },
] as const

function renderTabs(onChange: (id: string) => void, active = 'overview') {
  cleanup()
  return render(
    <Tabs tabs={TABS} active={active} onChange={onChange}>
      {(activeId) => <div>panel-{activeId}</div>}
    </Tabs>,
  )
}

// Controlled wrapper so selection actually changes when onChange fires.
function ControlledTabs() {
  const [active, setActive] = useState('overview')
  return (
    <Tabs tabs={TABS} active={active} onChange={setActive}>
      {(activeId) => <div>panel-{activeId}</div>}
    </Tabs>
  )
}

describe('Tabs', () => {
  it('renders a tablist with one tab per def and correct roles', () => {
    renderTabs(vi.fn())

    const tablist = screen.getByRole('tablist')
    expect(tablist).toBeTruthy()

    const tabs = screen.getAllByRole('tab')
    expect(tabs).toHaveLength(3)
    expect(tabs.map((t) => t.textContent)).toEqual(['Overview', 'Config', 'Logs'])
  })

  it('marks the active tab with aria-selected and roving tabindex', () => {
    renderTabs(vi.fn(), 'config')

    const tabs = screen.getAllByRole('tab')
    const [overview, config, logs] = tabs

    expect(config.getAttribute('aria-selected')).toBe('true')
    expect(overview.getAttribute('aria-selected')).toBe('false')
    expect(logs.getAttribute('aria-selected')).toBe('false')

    expect(config.getAttribute('tabindex')).toBe('0')
    expect(overview.getAttribute('tabindex')).toBe('-1')
    expect(logs.getAttribute('tabindex')).toBe('-1')
  })

  it('wires aria-controls / aria-labelledby between active tab and panel', () => {
    renderTabs(vi.fn(), 'config')

    const config = screen.getByRole('tab', { name: 'Config' })
    const panel = screen.getByRole('tabpanel')

    expect(config.getAttribute('aria-controls')).toBe(panel.getAttribute('id'))
    expect(panel.getAttribute('aria-labelledby')).toBe(config.getAttribute('id'))
  })

  it('renders only the active panel', () => {
    renderTabs(vi.fn(), 'config')

    expect(screen.getByText('panel-config')).toBeTruthy()
    expect(screen.queryByText('panel-overview')).toBeNull()
    expect(screen.queryByText('panel-logs')).toBeNull()
  })

  it('ArrowRight moves selection to the next tab (calling onChange)', () => {
    const onChange = vi.fn()
    renderTabs(onChange, 'overview')

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Overview' }), {
      key: 'ArrowRight',
    })

    expect(onChange).toHaveBeenCalledWith('config')
  })

  it('ArrowRight wraps from last to first', () => {
    const onChange = vi.fn()
    renderTabs(onChange, 'logs')

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Logs' }), {
      key: 'ArrowRight',
    })

    expect(onChange).toHaveBeenCalledWith('overview')
  })

  it('ArrowLeft moves to the previous tab and wraps', () => {
    const onChange = vi.fn()
    renderTabs(onChange, 'overview')

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Overview' }), {
      key: 'ArrowLeft',
    })

    expect(onChange).toHaveBeenCalledWith('logs')
  })

  it('Home selects the first tab, End selects the last', () => {
    const onChange = vi.fn()
    renderTabs(onChange, 'config')

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Config' }), {
      key: 'End',
    })
    expect(onChange).toHaveBeenCalledWith('logs')

    onChange.mockClear()
    fireEvent.keyDown(screen.getByRole('tab', { name: 'Config' }), {
      key: 'Home',
    })
    expect(onChange).toHaveBeenCalledWith('overview')
  })

  it('switches the visible panel when keyboard navigation changes selection', () => {
    cleanup()
    render(<ControlledTabs />)

    expect(screen.getByText('panel-overview')).toBeTruthy()

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Overview' }), {
      key: 'ArrowRight',
    })

    expect(screen.getByText('panel-config')).toBeTruthy()
    expect(screen.queryByText('panel-overview')).toBeNull()
  })
})
