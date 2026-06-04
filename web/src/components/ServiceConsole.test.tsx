// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ServiceConsole } from './ServiceConsole'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(async () => [
    {
      service_id: 'svc',
      service_name: 'web',
      deployment_id: 'dep',
      replica_index: 0,
      state: 'running',
    },
  ]),
}))

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 120
    rows = 32
    open() {}
    dispose() {}
    clear() {}
    focus() {}
    write() {}
    onData() {
      return { dispose() {} }
    }
  },
}))

describe('ServiceConsole', () => {
  it('renders replica selector and connect controls', async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    })
    render(
      <QueryClientProvider client={client}>
        <ServiceConsole serviceId="svc" />
      </QueryClientProvider>,
    )
    expect(await screen.findByLabelText('replica')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'connect' })).toBeTruthy()
  })
})
