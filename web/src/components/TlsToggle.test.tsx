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
  id: '018f1100-0000-7000-0000-000000000001',
  project_id: '018f1100-0000-7000-0000-000000000002',
  name: 'web',
  domains: ['example.com'],
  source: {
    type: 'external_image' as const,
    image: 'nginx:latest',
    credential: null,
    registry_id: null,
    image_ref: null,
  },
  internal_port: 3000,
  health_check: { path: '/healthz', timeout_seconds: 5 },
  env: [] as ReadonlyArray<readonly [string, string]>,
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
  it('uses verified domains when enabling tls for a service with stale empty domains', async () => {
    const { buildTlsTogglePayload } = await import('#/components/TlsToggle')

    expect(
      buildTlsTogglePayload(
        { ...FIXTURE_SERVICE, domains: [], tls_enabled: false },
        true,
        ['example.com', 'example.com'],
      ),
    ).toMatchObject({
      domains: ['example.com'],
      tls_enabled: true,
    })
  })

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
        expect.objectContaining({ queryKey: ['services', FIXTURE_SERVICE.id] }),
      )
      expect(invalidateSpy).toHaveBeenCalledWith(
        expect.objectContaining({ queryKey: ['ingress', 'routes'] }),
      )
    })

    invalidateSpy.mockRestore()
  })
})
