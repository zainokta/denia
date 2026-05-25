// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { vi } from 'vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { Login } from './login'
import { clearToken } from '../effect/auth-store'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(),
}))

vi.mock('@tanstack/react-router', async () => {
  const actual = await vi.importActual('@tanstack/react-router')
  return {
    ...actual,
    useNavigate: vi.fn(() => vi.fn()),
  }
})

import { runQuery } from '#/effect/runtime'
const mockRunQuery = runQuery as ReturnType<typeof vi.fn>

function renderLogin() {
  cleanup()
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <Login />
    </QueryClientProvider>,
  )
}

describe('Login route', () => {
  it('renders the login form', () => {
    clearToken()
    mockRunQuery.mockReset()
    mockRunQuery.mockResolvedValue({ token: 'test', expires_at: '2099-01-01' })
    renderLogin()
    expect(screen.getByLabelText('username')).toBeTruthy()
    expect(screen.getByLabelText('password')).toBeTruthy()
    expect(screen.getByRole('button', { name: /sign in/i })).toBeTruthy()
  })

  it('shows error on failed login', async () => {
    clearToken()
    mockRunQuery.mockReset()
    mockRunQuery.mockRejectedValue(new Error('invalid credentials'))

    renderLogin()
    fireEvent.change(screen.getByLabelText('username'), {
      target: { value: 'test' },
    })
    fireEvent.change(screen.getByLabelText('password'), {
      target: { value: 'pass' },
    })
    fireEvent.click(screen.getByRole('button', { name: /sign in/i }))

    const errorEl = await screen.findByText('invalid credentials')
    expect(errorEl).toBeTruthy()
  })
})
