// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { vi } from 'vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import React from 'react'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(),
}))

const FIXTURE_PROJECTS = [
  {
    id: '018f-p1',
    name: 'web',
    description: null,
    shared_env: [{ key: 'A', value: '1' }],
    default_resource_limits: null,
    created_at: '2026-05-25T00:00:00Z',
  },
  {
    id: '018f-p2',
    name: 'api',
    description: 'backend services',
    shared_env: [],
    default_resource_limits: { cpu_millis: 1000, memory_bytes: 536870912 },
    created_at: '2026-05-25T00:00:00Z',
  },
]

function renderProjects() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    React.createElement(
      QueryClientProvider,
      { client: queryClient },
      React.createElement(
        React.lazy(() => import('./index').then((m) => ({ default: m.ProjectsIndex }))),
      ),
    ),
  )
}

describe('Projects list route', () => {
  it('renders empty state when no projects', async () => {
    const { runQuery } = await import('#/effect/runtime')
    ;(runQuery as ReturnType<typeof vi.fn>).mockResolvedValueOnce([])

    renderProjects()
    expect(screen.getByText(/no projects/i)).toBeTruthy()
  })

  it('renders create form', async () => {
    const { runQuery } = await import('#/effect/runtime')
    ;(runQuery as ReturnType<typeof vi.fn>).mockResolvedValueOnce([])

    renderProjects()
    expect(screen.getByPlaceholderText('Project name')).toBeTruthy()
  })
})
