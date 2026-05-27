let memoryFallback = ''
const listeners = new Set<() => void>()

const KEY = 'denia_active_project'

function storage(): Storage | null {
  if (typeof window === 'undefined') return null
  return window.localStorage
}

export function getActiveProject(): string {
  const s = storage()
  if (s) {
    return s.getItem(KEY) ?? ''
  }
  return memoryFallback
}

export function setActiveProject(projectId: string): void {
  const s = storage()
  if (s) {
    if (projectId) {
      s.setItem(KEY, projectId)
    } else {
      s.removeItem(KEY)
    }
  } else {
    memoryFallback = projectId
  }
  for (const listener of listeners) listener()
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
