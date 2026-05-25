// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(() => Promise.resolve([])),
}))

import { runQuery } from '#/effect/runtime'
import { IngressRoute } from './ingress'

const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

const FIXTURE_ROUTES = [
  {
    service_name: 'web',
    domains: ['example.com'],
    bridge_port: 9090,
    tls: true,
  },
  {
    service_name: 'api',
    domains: ['api.example.com'],
    bridge_port: 9091,
    tls: false,
  },
]

const FIXTURE_CONFIG = 'http:\n  routers:\n    web:\n      service: web\n'

function makeWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return function TestWrapper({ children }: { children: React.ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>
        {children}
      </QueryClientProvider>
    )
  }
}

function allReturns(results: unknown[]) {
  let idx = 0
  mockRunQuery.mockImplementation(() => {
    const val = idx < results.length ? results[idx] : []
    idx++
    return Promise.resolve(val)
  })
}

describe('Ingress route', () => {
  it('renders route rows with TLS badge and http label', async () => {
    allReturns([FIXTURE_ROUTES, []])

    render(<IngressRoute />, { wrapper: makeWrapper() })

    expect(await screen.findByText('example.com')).toBeTruthy()
    expect(screen.getByText('api.example.com')).toBeTruthy()
    expect(screen.getByText('web')).toBeTruthy()
    expect(screen.getByText('api')).toBeTruthy()
    expect(screen.getByText('TLS')).toBeTruthy()
    expect(screen.getByText('http')).toBeTruthy()
  })

  it('renders empty state when no routes', async () => {
    allReturns([[], []])

    render(<IngressRoute />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No routes yet/)).toBeTruthy()
  })

  it('renders bridge ports in table', async () => {
    allReturns([FIXTURE_ROUTES, []])

    render(<IngressRoute />, { wrapper: makeWrapper() })

    expect(await screen.findByText('9090')).toBeTruthy()
    expect(screen.getByText('9091')).toBeTruthy()
  })

  it('shows raw YAML config on expand', async () => {
    allReturns([FIXTURE_ROUTES, FIXTURE_CONFIG])

    render(<IngressRoute />, { wrapper: makeWrapper() })

    await screen.findByText('example.com')

    const toggle = screen.getByText('raw config')
    fireEvent.click(toggle)

    expect(await screen.findByText(/routers:/)).toBeTruthy()
  })
})
