// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { ServiceForm } from './ServiceForm'
import type { Service, ServiceInput } from '#/effect/schema'

const projects = [
  { id: 'proj-1', name: 'alpha' },
  { id: 'proj-2', name: 'beta' },
]

function fill(label: string | RegExp, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
}

function addDomain(text: string) {
  const input = screen.getByLabelText(/^domains/i) as HTMLInputElement
  fireEvent.change(input, { target: { value: text } })
  fireEvent.keyDown(input, { key: 'Enter' })
}

afterEach(() => {
  cleanup()
})

describe('ServiceForm', () => {
  it('builds a well-formed external-image ServiceInput on submit', () => {
    const onSubmit = vi.fn<(value: ServiceInput | Service) => void>()
    render(<ServiceForm projects={projects} onSubmit={onSubmit} />)

    // external_image is the default source type
    fill('name', 'web')
    addDomain('example.com')
    fill('internal port', '8080')
    fill('image', 'nginx:latest')

    fireEvent.click(screen.getByRole('button', { name: /create service/i }))

    expect(onSubmit).toHaveBeenCalledTimes(1)
    const value = onSubmit.mock.calls[0]![0]

    // no id in create mode
    expect('id' in value).toBe(false)
    expect(value.name).toBe('web')
    expect(value.domains).toEqual(['example.com'])
    expect(value.internal_port).toBe(8080)
    expect(value.health_check.path.startsWith('/')).toBe(true)
    expect(value.health_check.timeout_seconds).toBeGreaterThan(0)
    expect(value.project_id).toBe('proj-1')

    expect(value.source.type).toBe('external_image')
    if (value.source.type === 'external_image') {
      expect(value.source.image).toBe('nginx:latest')
      expect(value.source.registry_id).toBeNull()
      expect(value.source.image_ref).toBeNull()
      expect(value.source.credential).toBeNull()
    }
  })

  it('disables submit until name, port and source are valid (domain is optional)', () => {
    const onSubmit = vi.fn<(value: ServiceInput | Service) => void>()
    render(<ServiceForm projects={projects} onSubmit={onSubmit} />)

    const submit = screen.getByRole('button', { name: /create service/i })
    const isDisabled = () => submit.hasAttribute('disabled')

    // nothing filled -> disabled
    expect(isDisabled()).toBe(true)

    // name only -> still disabled (port invalid, no source)
    fill('name', 'web')
    expect(isDisabled()).toBe(true)

    // + port -> still disabled (no source image)
    fill('internal port', '8080')
    expect(isDisabled()).toBe(true)

    // + image -> now valid without a domain
    fill('image', 'nginx:latest')
    expect(isDisabled()).toBe(false)

    // clearing name re-disables
    fill('name', '')
    expect(isDisabled()).toBe(true)
  })

  it('submit is enabled without a domain, TLS checkbox is disabled when no domain', () => {
    const onSubmit = vi.fn<(value: ServiceInput | Service) => void>()
    render(<ServiceForm projects={projects} onSubmit={onSubmit} />)

    fill('name', 'api')
    fill('internal port', '3000')
    fill('image', 'nginx:latest')

    const submit = screen.getByRole('button', { name: /create service/i })
    expect(submit.hasAttribute('disabled')).toBe(false)

    const tls = screen.getByLabelText('TLS enabled') as HTMLInputElement
    expect(tls.disabled).toBe(true)
    expect(tls.checked).toBe(false)
  })

  it('TLS checkbox becomes enabled when a domain is typed', () => {
    render(<ServiceForm projects={projects} onSubmit={vi.fn()} />)

    const tls = screen.getByLabelText('TLS enabled') as HTMLInputElement
    expect(tls.disabled).toBe(true)

    addDomain('example.com')

    expect(tls.disabled).toBe(false)
  })

  it('submits with empty domains and tls_enabled=false when no domain is given', () => {
    const onSubmit = vi.fn<(value: ServiceInput | Service) => void>()
    render(<ServiceForm projects={projects} onSubmit={onSubmit} />)

    fill('name', 'svc')
    fill('internal port', '8080')
    fill('image', 'nginx:latest')

    fireEvent.click(screen.getByRole('button', { name: /create service/i }))

    expect(onSubmit).toHaveBeenCalledTimes(1)
    const value = onSubmit.mock.calls[0]![0]
    expect(value.domains).toEqual([])
    expect(value.tls_enabled).toBe(false)
  })
})
