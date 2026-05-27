let memoryFallback: string | undefined = undefined
const listeners = new Set<() => void>()

const KEY = 'denia_token'

function storage(): Storage | null {
  if (typeof window === 'undefined') return null
  return window.sessionStorage
}

export function getToken(): string | undefined {
  const s = storage()
  if (s) {
    const value = s.getItem(KEY)
    return value ?? undefined
  }
  return memoryFallback
}

export function setToken(token: string): void {
  const s = storage()
  if (s) {
    s.setItem(KEY, token)
  } else {
    memoryFallback = token
  }
  for (const listener of listeners) listener()
}

export function clearToken(): void {
  const s = storage()
  if (s) {
    s.removeItem(KEY)
  } else {
    memoryFallback = undefined
  }
  for (const listener of listeners) listener()
}

export function captureTokenFromUrl(): void {
  if (typeof window === 'undefined') return
  const params = new URLSearchParams(window.location.search)
  const token = params.get('token')
  if (token) {
    setToken(token)
    params.delete('token')
    const qs = params.toString()
    const url =
      window.location.pathname + (qs ? `?${qs}` : '') + window.location.hash
    window.history.replaceState({}, '', url)
  }
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
