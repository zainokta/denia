// @vitest-environment jsdom
import { describe, expect, it } from '@effect/vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook } from '@testing-library/react'
import { can, useAuth } from './useAuth'
import { clearToken } from '../effect/auth-store'

function queryWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  )
}

describe('can', () => {
  it('admin can do anything', () => {
    expect(can('viewer', 'admin')).toBe(true)
    expect(can('operator', 'admin')).toBe(true)
    expect(can('admin', 'admin')).toBe(true)
  })

  it('operator can do viewer and operator actions', () => {
    expect(can('viewer', 'operator')).toBe(true)
    expect(can('operator', 'operator')).toBe(true)
    expect(can('admin', 'operator')).toBe(false)
  })

  it('viewer can only do viewer actions', () => {
    expect(can('viewer', 'viewer')).toBe(true)
    expect(can('operator', 'viewer')).toBe(false)
    expect(can('admin', 'viewer')).toBe(false)
  })
})

describe('useAuth', () => {
  it('returns undefined me when no token is set', () => {
    clearToken()
    const { result } = renderHook(() => useAuth(), {
      wrapper: queryWrapper(),
    })
    expect(result.current.token).toBeUndefined()
    expect(result.current.me).toBeUndefined()
    expect(result.current.isBootstrap).toBe(false)
    expect(result.current.isSuperAdmin).toBe(false)
  })

  it('returns undefined role when me is not loaded', () => {
    clearToken()
    const { result } = renderHook(() => useAuth(), {
      wrapper: queryWrapper(),
    })
    expect(result.current.roleForActiveProject(1)).toBeUndefined()
  })
})
