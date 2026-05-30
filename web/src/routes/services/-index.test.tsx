// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi, beforeEach } from 'vitest'
import {
  cleanup,
  render,
  screen,
  fireEvent,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  ServicesIndex,
  listServices,
  listWorkloads,
  listProjects,
} from './index'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(() => Promise.resolve([])),
}))

// <Link> needs router context (useLinkProps reads it); stub it to a plain
// anchor so ServicesIndex renders in jsdom without a RouterProvider. Mirrors
// the stub used in services/-detail.test.tsx.
vi.mock('@tanstack/react-router', async () => {
  const actual = await vi.importActual('@tanstack/react-router')
  return {
    ...actual,
    useNavigate: vi.fn(() => vi.fn()),
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

import { runQuery } from '#/effect/runtime'

const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

const SVC_A = '0190b8a0-0000-7000-8000-000000000001'
const SVC_B = '0190b8a0-0000-7000-8000-000000000002'
const PROJECT = '0190b8a0-0000-7000-8000-0000000000aa'

const FIXTURE_SERVICES = [
  {
    id: SVC_A,
    project_id: PROJECT,
    name: 'web',
    domains: ['example.com'],
    source: {
      type: 'external_image',
      image: 'nginx:latest',
      credential: null,
      registry_id: null,
      image_ref: null,
    },
    internal_port: 3000,
    health_check: { path: '/', timeout_seconds: 5 },
    resource_limits: null,
    env: [],
    tls_enabled: true,
  },
  {
    id: SVC_B,
    project_id: PROJECT,
    name: 'api',
    domains: ['api.example.com'],
    source: {
      type: 'external_image',
      image: 'api:latest',
      credential: null,
      registry_id: null,
      image_ref: null,
    },
    internal_port: 8080,
    health_check: { path: '/health', timeout_seconds: 5 },
    resource_limits: null,
    env: [],
    tls_enabled: false,
  },
]

const FIXTURE_WORKLOADS = [
  {
    service_id: SVC_A,
    service_name: 'web',
    project_id: PROJECT,
    deployment_id: null,
    status: 'Healthy',
    cpu_usage_usec: null,
    memory_current_bytes: null,
  },
  {
    service_id: SVC_B,
    service_name: 'api',
    project_id: PROJECT,
    deployment_id: null,
    status: 'Failed',
    cpu_usage_usec: null,
    memory_current_bytes: null,
  },
]

const FIXTURE_PROJECTS = [
  {
    id: PROJECT,
    name: 'demo',
    description: null,
    shared_env: [],
    default_resource_limits: null,
    created_at: '2026-05-25T00:00:00Z',
  },
]

// Dispatch runQuery by the effect identity passed in.
function dispatch(effect: unknown): Promise<unknown> {
  if (effect === listServices) return Promise.resolve(FIXTURE_SERVICES)
  if (effect === listWorkloads) return Promise.resolve(FIXTURE_WORKLOADS)
  if (effect === listProjects) return Promise.resolve(FIXTURE_PROJECTS)
  return Promise.resolve(undefined)
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
})

describe('ServicesIndex', () => {
  beforeEach(() => {
    mockRunQuery.mockReset()
    mockRunQuery.mockImplementation((effect: unknown) => dispatch(effect))
  })

  it('renders service rows with derived status', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    expect(await screen.findByText('web')).toBeTruthy()
    expect(screen.getByText('api')).toBeTruthy()
    // StatusSignal renders the raw phase text.
    expect(screen.getByText('Healthy')).toBeTruthy()
    expect(screen.getByText('Failed')).toBeTruthy()
  })

  it('renders empty state when no services', async () => {
    mockRunQuery.mockImplementation(() => Promise.resolve([]))

    render(<ServicesIndex />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No services yet/)).toBeTruthy()
  })

  it('has deploy and stop buttons per row', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')

    expect(screen.getAllByText('deploy').length).toBe(2)
    expect(screen.getAllByText('stop').length).toBe(2)
  })

  it('opens the create disclosure and renders ServiceForm', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')
    fireEvent.click(screen.getByText('new service'))

    // ServiceForm-specific control.
    expect(screen.getByText('source type')).toBeTruthy()
    expect(screen.getByText('create service')).toBeTruthy()
  })

  it('submits the create form via the putService effect path', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')
    fireEvent.click(screen.getByText('new service'))

    // Fill the minimum required fields for the form to be valid.
    fireEvent.change(screen.getByLabelText('name'), {
      target: { value: 'newsvc' },
    })
    fireEvent.change(screen.getByLabelText(/^domains/i), {
      target: { value: 'new.example.com' },
    })
    fireEvent.change(screen.getByLabelText('internal port'), {
      target: { value: '9000' },
    })
    fireEvent.change(screen.getByLabelText('image'), {
      target: { value: 'newsvc:latest' },
    })

    mockRunQuery.mockClear()
    fireEvent.click(screen.getByText('create service'))

    // The first effect run on submit is the putService / createService
    // mutation path, distinct from the list query effects. (A `services`
    // refetch follows via invalidateQueries on success.)
    await waitFor(() => expect(mockRunQuery).toHaveBeenCalled())
    const submitted = mockRunQuery.mock.calls[0]?.[0]
    expect(submitted).not.toBe(listServices)
    expect(submitted).not.toBe(listWorkloads)
    expect(submitted).not.toBe(listProjects)
  })

  it('confirms and deletes a service via the deleteService effect path', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')

    fireEvent.click(screen.getAllByText('delete')[0]!)
    expect(screen.getByText('delete?')).toBeTruthy()

    mockRunQuery.mockClear()
    fireEvent.click(screen.getByText('yes'))

    // The first effect run on confirm is the deleteService mutation path,
    // distinct from the list query effects. (A `services` refetch follows
    // via invalidateQueries on success.)
    await waitFor(() => expect(mockRunQuery).toHaveBeenCalled())
    const submitted = mockRunQuery.mock.calls[0]?.[0]
    expect(submitted).not.toBe(listServices)
    expect(submitted).not.toBe(listWorkloads)
    expect(submitted).not.toBe(listProjects)
  })
})
