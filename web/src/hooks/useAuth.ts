import { useSyncExternalStore } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Effect } from 'effect'
import { getToken, setToken, clearToken, subscribe } from '../effect/auth-store'
import { runQuery } from '../effect/runtime'
import { ApiClient } from '../effect/api-client'
import type { Role } from '../effect/schema'

const ROLE_RANK: Record<Role, number> = {
  viewer: 0,
  operator: 1,
  admin: 2,
}

export function can(required: Role, userRole: Role): boolean {
  return ROLE_RANK[userRole] >= ROLE_RANK[required]
}

export function useAuth() {
  const queryClient = useQueryClient()

  const token = useSyncExternalStore(subscribe, getToken, getToken)

  const meQuery = useQuery({
    queryKey: ['me'],
    queryFn: () =>
      runQuery(
        Effect.gen(function* () {
          const api = yield* ApiClient
          return yield* api.me
        }),
      ),
    enabled: typeof token === 'string' && token.length > 0,
    staleTime: 5 * 60 * 1000,
    retry: false,
  })

  const roleForActiveProject = (
    projectId: number,
  ): Role | undefined => {
    if (!meQuery.data) return undefined
    if (meQuery.data.is_super_admin) return 'admin'
    const membership = meQuery.data.memberships.find(
      (m) => m.project_id === projectId,
    )
    return membership?.role
  }

  const loginMutation = useMutation({
    mutationFn: (creds: { username: string; password: string }) =>
      runQuery(
        Effect.gen(function* () {
          const api = yield* ApiClient
          return yield* api.login(creds.username, creds.password)
        }),
      ),
    onSuccess: (data) => {
      setToken(data.token)
      queryClient.invalidateQueries({ queryKey: ['me'] })
    },
  })

  const logoutMutation = useMutation({
    mutationFn: () =>
      runQuery(
        Effect.gen(function* () {
          const api = yield* ApiClient
          return yield* api.logout
        }),
      ),
    onSettled: () => {
      clearToken()
      queryClient.removeQueries({ queryKey: ['me'] })
    },
  })

  return {
    token,
    me: meQuery.data,
    isLoading: meQuery.isLoading,
    isError: meQuery.error,
    isBootstrap: meQuery.data?.principal.kind === 'bootstrap',
    isSuperAdmin: meQuery.data?.is_super_admin ?? false,
    roleForActiveProject,
    login: loginMutation.mutateAsync,
    logout: logoutMutation.mutateAsync,
    loginStatus: loginMutation.status,
    loginError: loginMutation.error,
  } as const
}
