// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { ServiceForm } from './ServiceForm'
import type { Service, ServiceInput } from '#/effect/schema'

const projects = [
  { id: 'proj-1', name: 'alpha' },
  { id: 'proj-2', name: 'beta' },
]

function fill(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
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
    fill('domains', 'example.com')
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

  it('disables submit until name, a domain, port and source are valid', () => {
    const onSubmit = vi.fn<(value: ServiceInput | Service) => void>()
    render(<ServiceForm projects={projects} onSubmit={onSubmit} />)

    const submit = screen.getByRole('button', { name: /create service/i })
    const isDisabled = () => submit.hasAttribute('disabled')

    // nothing filled -> disabled
    expect(isDisabled()).toBe(true)

    // name only -> still disabled
    fill('name', 'web')
    expect(isDisabled()).toBe(true)

    // + domain -> still disabled (port is 0, no source)
    fill('domains', 'example.com')
    expect(isDisabled()).toBe(true)

    // + port -> still disabled (no source image)
    fill('internal port', '8080')
    expect(isDisabled()).toBe(true)

    // + image -> now valid
    fill('image', 'nginx:latest')
    expect(isDisabled()).toBe(false)

    // clearing name re-disables
    fill('name', '')
    expect(isDisabled()).toBe(true)
  })
})
