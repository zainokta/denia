import { MutationCache, QueryCache, QueryClient } from '@tanstack/react-query'
import { clearToken } from '#/effect/auth-store'

// Walk the error (and its `cause` chain) for an HTTP status. ApiError carries
// `status` directly; runtime failures may wrap it. Depth-bounded.
function getStatus(error: unknown, depth = 0): number | undefined {
  if (!error || typeof error !== 'object' || depth > 6) return undefined
  const rec = error as Record<string, unknown>
  if (typeof rec.status === 'number') return rec.status
  return getStatus(rec.cause, depth + 1)
}

function isUnauthorized(error: unknown): boolean {
  return getStatus(error) === 401
}

// A stale or revoked token passes the presence-only route gate, then every
// query 401s. On the first unauthorized response, drop the token and bounce to
// /login. Guarded so concurrent 401s don't fire multiple navigations, and a
// full-document load clears all cached state cleanly.
let redirecting = false
function handleUnauthorized() {
  if (typeof window === 'undefined') return
  clearToken()
  // Already on /login (e.g. a bad-credentials 401): clear and let the page show
  // its own error. The latch is set only when we actually navigate, so it can't
  // suppress a future redirect after a successful login on the same document.
  if (window.location.pathname !== '/login' && !redirecting) {
    redirecting = true
    window.location.assign('/login')
  }
}

export function getContext() {
  const queryClient = new QueryClient({
    queryCache: new QueryCache({
      onError: (error) => {
        if (isUnauthorized(error)) handleUnauthorized()
      },
    }),
    mutationCache: new MutationCache({
      onError: (error) => {
        if (isUnauthorized(error)) handleUnauthorized()
      },
    }),
    defaultOptions: {
      queries: {
        // Never retry an auth failure (it would only restart the 401 storm).
        retry: (count, error) => !isUnauthorized(error) && count < 2,
      },
    },
  })

  return {
    queryClient,
  }
}
export default function TanstackQueryProvider() {}
