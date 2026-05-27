import { useSyncExternalStore } from 'react'
import {
  getActiveProject,
  setActiveProject,
  subscribe,
} from '../effect/active-project-store'

export function useActiveProject(): [string, (projectId: string) => void] {
  const activeProject = useSyncExternalStore(
    subscribe,
    getActiveProject,
    getActiveProject,
  )
  return [activeProject, setActiveProject]
}
