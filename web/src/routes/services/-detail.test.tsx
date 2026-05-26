// @vitest-environment jsdom
import { describe, expect, it, vi, afterEach } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

afterEach(() => {
  cleanup()
})

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(() => Promise.resolve([])),
}))

vi.mock('@tanstack/react-router', async () => {
  const actual = await vi.importActual('@tanstack/react-router')
  return {
    ...actual,
    useParams: vi.fn(() => ({ serviceId: '1' })),
  }
})

vi.mock('#/hooks/useAuth', () => ({
  useAuth: () => ({
    isSuperAdmin: true,
    roleForActiveProject: () => 'admin',
    token: 'test',
    me: undefined,
  }),
  can: (_required: string, _userRole: string) => true,
}))

import { runQuery } from '#/effect/runtime'
import { ServiceDetail } from './\$serviceId'

const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

const fixDeployments = [
  { id: 5, service_id: 1, status: 'Failed', created_at: '2026-05-25T02:00:00Z' },
  { id: 1, service_id: 1, status: 'Healthy', created_at: '2026-05-25T00:00:00Z' },
]

const fixLogs = [
  '2026-05-25T00:00:00Z [init] starting',
  '2026-05-25T00:00:01Z [http] listening on :3000',
]

const fixMetrics = [
  { service_id: 1, cpu_percent: 0.45, memory_bytes: 268435456, recorded_at: '2026-05-25T00:00:00Z' },
  { service_id: 1, cpu_percent: 0.12, memory_bytes: 134217728, recorded_at: '2026-05-25T00:01:00Z' },
]

const fixServices = [
  { id: 1, project_id: 42, name: 'web', domains: ['example.com'], internal_port: 3000 },
]

const fixServicesWithSecurity = [
  {
    id: 1,
    project_id: 42,
    name: 'web',
    domains: ['example.com'],
    internal_port: 3000,
    security: {
      userns: true,
      mapped_uid: 100000,
      no_new_privs: true,
      caps_dropped: true,
    },
  },
]

function makeWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
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

describe('ServiceDetail', () => {
  it('renders deployments newest first with status signals', async () => {
    allReturns([fixDeployments, [], []])
    // 4th call (services) falls through to default []

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText('Healthy')).toBeTruthy()
    expect(screen.getByText('Failed')).toBeTruthy()
  })

  it('renders no-deployments empty state', async () => {
    allReturns([[], [], []])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No deployments yet/)).toBeTruthy()
  })

  it('renders logs with line numbers', async () => {
    allReturns([fixDeployments, fixLogs, []])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/\[init\]/)).toBeTruthy()
    expect(screen.getByText(/\[http\]/)).toBeTruthy()
  })

  it('renders empty logs state', async () => {
    allReturns([fixDeployments, [], []])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No logs available/)).toBeTruthy()
  })

  it('renders metrics table with formatted values', async () => {
    allReturns([fixDeployments, [], fixMetrics])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText('45.0%')).toBeTruthy()
    expect(screen.getByText('256.0 MiB')).toBeTruthy()
    expect(screen.getByText('12.0%')).toBeTruthy()
    expect(screen.getByText('128.0 MiB')).toBeTruthy()
  })

  it('renders empty metrics state', async () => {
    allReturns([fixDeployments, [], []])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No metrics available/)).toBeTruthy()
  })

  it('has deploy and stop buttons', async () => {
    allReturns([fixDeployments, [], []])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText('deploy')).toBeTruthy()
    expect(screen.getByText('stop')).toBeTruthy()
  })

  it('renders DeployPhase stepline for newest deployment', async () => {
    const deployFix = [
      { id: 1, service_id: 1, status: 'Building', created_at: '2026-05-25T00:00:00Z' },
    ]
    allReturns([deployFix, [], [], [], fixServices])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    await screen.findByText('queued')
    const warn = document.querySelector('.signal-warn')
    expect(warn).toBeTruthy()
  })

  it('renders artifact digest when present', async () => {
    const deployFix = [
      {
        id: 1,
        service_id: 1,
        status: 'Healthy',
        created_at: '2026-05-25T00:00:00Z',
        artifact: { digest: 'sha256:abc123def456', kind: 'OciImage' },
      },
    ]
    allReturns([deployFix, [], [], [], fixServices])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/sha256:abc1/)).toBeTruthy()
    expect(screen.getByText('image')).toBeTruthy()
  })

  it('shows artifact pending when not present', async () => {
    const deployFix = [
      {
        id: 1,
        service_id: 1,
        status: 'Building',
        created_at: '2026-05-25T00:00:00Z',
      },
    ]
    allReturns([deployFix, [], [], [], fixServices])

    render(<ServiceDetail />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/artifact/)).toBeTruthy()
  })

  it('renders posture panel with all protections', async () => {
    const deployFix = [
      { id: 1, service_id: 1, status: 'Healthy', created_at: '2026-05-25T00:00:00Z' },
    ]
    allReturns([deployFix, [], [], [], fixServicesWithSecurity])

    const { container } = render(<ServiceDetail />, { wrapper: makeWrapper() })

    await screen.findByText('Healthy')
    expect(container.textContent).toContain('userns')
    expect(container.textContent).toContain('100000')
    expect(container.textContent).toContain('no_new_privs')
    expect(container.textContent).toContain('caps')
    expect(container.textContent).toContain('sandboxed')
  })

  it('no posture panel when service has no security', async () => {
    const deployFix = [
      { id: 1, service_id: 1, status: 'Healthy', created_at: '2026-05-25T00:00:00Z' },
    ]
    allReturns([deployFix, [], [], [], fixServices])

    const { container } = render(<ServiceDetail />, { wrapper: makeWrapper() })

    await screen.findByText('Healthy')
    expect(container.textContent).toContain('posture: n/a')
  })
})
