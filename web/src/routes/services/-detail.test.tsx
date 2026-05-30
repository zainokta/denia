// @vitest-environment jsdom
import { describe, expect, it, vi, afterEach } from 'vitest'
import { render, screen, cleanup, fireEvent } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

afterEach(() => {
  cleanup()
  logsState.lines = []
  logsState.error = null
})

const navigateMock = vi.fn()

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(() => Promise.resolve([])),
}))

vi.mock('@tanstack/react-router', async () => {
  const actual = await vi.importActual('@tanstack/react-router')
  return {
    ...actual,
    useParams: vi.fn(() => ({ serviceId: 's-1' })),
    useNavigate: vi.fn(() => navigateMock),
    // <Link> needs router context (useLinkProps reads it); stub it to a plain
    // anchor so ServiceDetail renders in jsdom without a RouterProvider.
    Link: ({ children }: { children?: React.ReactNode }) => (
      <a href="#">{children}</a>
    ),
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

// The Logs tab renders <LogStream>, which streams over useLogStream (an SSE
// fetch with no base URL in jsdom). Stub the hook with settable state so the
// Logs tab can be tested directly.
const logsState = vi.hoisted(() => ({
  lines: [] as string[],
  error: null as string | null,
}))

vi.mock('#/hooks/useLogStream', () => ({
  useLogStream: () => ({
    lines: logsState.lines.map((text, seq) => ({ seq, text })),
    status: logsState.error ? 'error' : 'streaming',
    error: logsState.error,
    clear: () => {},
    reconnect: () => {},
  }),
}))

import { runQuery } from '#/effect/runtime'
import { ServiceDetail } from './\$serviceId'

const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

const fixService = {
  id: 's-1',
  project_id: 'p-42',
  name: 'web',
  domains: ['example.com'],
  source: {
    type: 'git' as const,
    repo_url: 'https://example.com/repo.git',
    git_ref: 'main',
    dockerfile_path: 'Dockerfile',
    context_path: '.',
    credential: { name: 'deploy', key: 'ssh' },
  },
  internal_port: 3000,
  health_check: { path: '/', timeout_seconds: 5 },
  resource_limits: null,
  env: [['LOG_LEVEL', 'info'] as [string, string]],
  tls_enabled: true,
}

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

// runQuery call order in ServiceDetail (logs are NOT a runQuery call — they come
// from the useServiceLogs hook, mocked above):
//   getService, listProjects, deployments, metrics, requests, domains.
// Map each to a fixture; anything unspecified defaults to [].
interface Returns {
  service?: unknown
  projects?: unknown
  deployments?: unknown
  metrics?: unknown
  requests?: unknown
  domains?: unknown
}

function setReturns(r: Returns) {
  const sequence = [
    r.service ?? fixService,
    r.projects ?? [],
    r.deployments ?? [],
    r.metrics ?? [],
    r.requests ?? [],
    r.domains ?? [],
  ]
  let idx = 0
  mockRunQuery.mockImplementation(() => {
    const val = idx < sequence.length ? sequence[idx] : []
    idx++
    return Promise.resolve(val)
  })
}

describe('ServiceDetail', () => {
  it('renders the service name in the header', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    expect(await screen.findByRole('heading', { name: 'web' })).toBeTruthy()
  })

  it('shows overview tab by default with port and tls', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })
    expect(screen.getByText('3000')).toBeTruthy()
    expect(screen.getByText('enabled')).toBeTruthy()
  })

  it('shows autoscale as disabled on overview when no policy is set', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })
    expect(screen.getByText('disabled')).toBeTruthy()
  })

  it('summarizes the autoscale policy on overview when set', async () => {
    setReturns({
      service: {
        ...fixService,
        autoscale: {
          min_replicas: 1,
          max_replicas: 3,
          target_cpu_pct: 70,
          target_mem_pct: 60,
          scale_down_cooldown_s: 300,
          idle_timeout_s: 600,
        },
      },
    })
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })
    expect(screen.getByText(/replicas · 70% cpu · 60% mem/)).toBeTruthy()
  })

  it('flags scale-to-zero on overview when min replicas is 0', async () => {
    setReturns({
      service: {
        ...fixService,
        autoscale: {
          min_replicas: 0,
          max_replicas: 2,
          target_cpu_pct: 70,
          target_mem_pct: null,
          scale_down_cooldown_s: 300,
          idle_timeout_s: 600,
        },
      },
    })
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })
    expect(screen.getByText(/scale-to-zero/)).toBeTruthy()
  })

  it('shows source summary when the Source tab is selected', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    fireEvent.click(screen.getByRole('tab', { name: 'source' }))
    expect(await screen.findByText('https://example.com/repo.git')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'edit' })).toBeTruthy()
  })

  it('shows logs when the Logs tab is selected', async () => {
    setReturns({})
    logsState.lines = fixLogs
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    fireEvent.click(screen.getByRole('tab', { name: 'logs' }))
    expect(await screen.findByText(/\[init\]/)).toBeTruthy()
    expect(screen.getByText(/\[http\]/)).toBeTruthy()
  })

  it('shows metrics when the Metrics tab is selected', async () => {
    setReturns({ metrics: fixMetrics })
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    fireEvent.click(screen.getByRole('tab', { name: 'metrics' }))
    expect(await screen.findByText('45.0%')).toBeTruthy()
    expect(screen.getByText('256.0 MiB')).toBeTruthy()
  })

  it('shows deployments when the Deployments tab is selected', async () => {
    setReturns({ deployments: fixDeployments })
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    fireEvent.click(screen.getByRole('tab', { name: 'deployments' }))
    expect(await screen.findByText('Healthy')).toBeTruthy()
    expect(screen.getByText('Failed')).toBeTruthy()
  })

  it('renders env table on the Environment tab', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    fireEvent.click(screen.getByRole('tab', { name: 'environment' }))
    expect(await screen.findByText('LOG_LEVEL')).toBeTruthy()
    expect(screen.getByText('info')).toBeTruthy()
  })

  it('has deploy and stop buttons in the header', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    expect(await screen.findByText('deploy')).toBeTruthy()
    expect(screen.getByText('stop')).toBeTruthy()
  })

  it('deletes the service after confirming and navigates back', async () => {
    setReturns({})
    render(<ServiceDetail />, { wrapper: makeWrapper() })
    await screen.findByRole('heading', { name: 'web' })

    mockRunQuery.mockClear()
    mockRunQuery.mockResolvedValue(undefined)

    fireEvent.click(screen.getByRole('button', { name: 'delete' }))
    fireEvent.click(
      await screen.findByRole('button', { name: 'confirm delete service' }),
    )

    // deleteService -> runQuery was invoked
    await vi.waitFor(() => {
      expect(mockRunQuery).toHaveBeenCalled()
      expect(navigateMock).toHaveBeenCalledWith({ to: '/services' })
    })
  })
})
