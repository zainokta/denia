// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import { vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn((_e: unknown) => Promise.resolve()),
}))

import { runQuery } from '#/effect/runtime'

const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

const FIXTURE_SERVICE = {
  id: 1,
  project_id: 42,
  name: 'web',
  domains: ['example.com'],
  internal_port: 3000,
  tls_enabled: false,
}

function makeWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  })
  return function TestWrapper({ children }: { children: React.ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>
        {children}
      </QueryClientProvider>
    )
  }
}

afterEach(() => {
  cleanup()
  mockRunQuery.mockReset()
})

describe('TlsToggle', () => {
  it('renders with current TLS state', async () => {
    const { TlsToggle } = await import('#/components/TlsToggle')
    render(<TlsToggle service={FIXTURE_SERVICE} />, { wrapper: makeWrapper() })

    expect(await screen.findByText('Enable TLS')).toBeTruthy()
    expect(screen.queryByText('http')).toBeTruthy()
  })

  it('shows TLS badge when enabled', async () => {
    const { TlsToggle } = await import('#/components/TlsToggle')
    render(
      <TlsToggle service={{ ...FIXTURE_SERVICE, tls_enabled: true }} />,
      { wrapper: makeWrapper() },
    )

    expect(await screen.findByText('Disable TLS')).toBeTruthy()
    expect(screen.queryByText('TLS')).toBeTruthy()
  })

  it('toggles tls_enabled and invalidates queries', async () => {
    mockRunQuery.mockResolvedValue({ ...FIXTURE_SERVICE, tls_enabled: true })

    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false },
        mutations: { retry: false },
      },
    })
    const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries')

    const { TlsToggle } = await import('#/components/TlsToggle')
    render(
      <TlsToggle service={FIXTURE_SERVICE} />,
      {
        wrapper: function Wrapper({ children }: { children: React.ReactNode }) {
          return (
            <QueryClientProvider client={queryClient}>
              {children}
            </QueryClientProvider>
          )
        },
      },
    )

    const button = await screen.findByText('Enable TLS')
    fireEvent.click(button)

    await vi.waitFor(() => {
      expect(invalidateSpy).toHaveBeenCalledWith(
        expect.objectContaining({ queryKey: ['services'] }),
      )
      expect(invalidateSpy).toHaveBeenCalledWith(
        expect.objectContaining({ queryKey: ['ingress', 'routes'] }),
      )
    })

    invalidateSpy.mockRestore()
  })
})
