// @vitest-environment jsdom
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ServicesIndex } from './index'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(() => Promise.resolve([])),
}))

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

const FIXTURE_SERVICES = [
  {
    id: 1,
    project_id: 42,
    name: 'web',
    domains: ['example.com'],
    internal_port: 3000,
    status: 'Healthy',
    security: {
      userns: true,
      mapped_uid: 100000,
      no_new_privs: true,
      caps_dropped: true,
    },
  },
  {
    id: 2,
    project_id: 42,
    name: 'api',
    domains: ['api.example.com'],
    internal_port: 8080,
    status: 'Failed',
  },
]

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

describe('ServicesIndex', () => {
  beforeEach(() => {
    mockRunQuery.mockReset()
    mockRunQuery.mockImplementation(() =>
      new Promise((r) => setTimeout(() => r([]), 0)),
    )
  })

  it('renders services with status signals', async () => {
    mockRunQuery.mockImplementation(() =>
      Promise.resolve(FIXTURE_SERVICES),
    )

    render(<ServicesIndex />, { wrapper: makeWrapper() })

    expect(await screen.findByText('web')).toBeTruthy()
    expect(screen.getByText('api')).toBeTruthy()
    expect(screen.getByText('Healthy')).toBeTruthy()
    expect(screen.getByText('Failed')).toBeTruthy()
  })

  it('renders empty state when no services', async () => {
    render(<ServicesIndex />, { wrapper: makeWrapper() })

    expect(await screen.findByText(/No services yet/)).toBeTruthy()
  })

  it('has deploy and stop buttons', async () => {
    mockRunQuery.mockImplementation(() =>
      Promise.resolve(FIXTURE_SERVICES),
    )

    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')

    expect(screen.getAllByText('deploy').length).toBe(2)
    expect(screen.getAllByText('stop').length).toBe(2)
  })

  it('shows sandboxed badge for service with full posture', async () => {
    mockRunQuery.mockImplementation(() =>
      Promise.resolve(FIXTURE_SERVICES),
    )

    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findByText('web')
    const badges = screen.queryAllByText('sandboxed')
    expect(badges.length).toBeGreaterThanOrEqual(1)
  })

  it('shows posture n/a for service without security', async () => {
    mockRunQuery.mockImplementation(() =>
      Promise.resolve(FIXTURE_SERVICES),
    )

    render(<ServicesIndex />, { wrapper: makeWrapper() })

    await screen.findAllByText('web')
    const naTexts = screen.queryAllByText(/n\/a/)
    expect(naTexts.length).toBeGreaterThanOrEqual(1)
  })
})
