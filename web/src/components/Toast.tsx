import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { X } from 'lucide-react'

export type ToastTone = 'default' | 'error' | 'ok'

interface ToastItem {
  readonly id: number
  readonly message: string
  readonly tone: ToastTone
}

interface ToastApi {
  readonly push: (message: string, tone?: ToastTone) => void
}

const ToastContext = createContext<ToastApi | null>(null)

const DISMISS_MS = 4500

export function ToastProvider({ children }: { readonly children: ReactNode }) {
  const [items, setItems] = useState<ReadonlyArray<ToastItem>>([])
  const idRef = useRef(0)

  const remove = useCallback((id: number) => {
    setItems((prev) => prev.filter((t) => t.id !== id))
  }, [])

  const push = useCallback(
    (message: string, tone: ToastTone = 'default') => {
      const id = idRef.current++
      setItems((prev) => [...prev, { id, message, tone }])
      setTimeout(() => remove(id), DISMISS_MS)
    },
    [remove],
  )

  const api = useMemo<ToastApi>(() => ({ push }), [push])

  return (
    <ToastContext.Provider value={api}>
      {children}
      {/* Polite live region: announces without stealing focus. */}
      <div className="toast-host" role="region" aria-live="polite" aria-label="Notifications">
        {items.map((t) => (
          <div
            key={t.id}
            className={`toast ${t.tone === 'error' ? 'is-error' : t.tone === 'ok' ? 'is-ok' : ''}`}
          >
            <span style={{ minWidth: 0, wordBreak: 'break-word' }}>{t.message}</span>
            <button
              type="button"
              className="toast-close"
              aria-label="Dismiss"
              onClick={() => remove(t.id)}
            >
              <X size={14} aria-hidden="true" />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  )
}

// Returns a no-op-safe toaster. Outside a provider, push() is inert rather
// than throwing, so leaf components stay decoupled.
export function useToast(): ToastApi {
  const ctx = useContext(ToastContext)
  const fallback = useMemo<ToastApi>(() => ({ push: () => undefined }), [])
  return ctx ?? fallback
}

// Convenience: surface a success or error toast for a settled async action.
export function useActionToasts() {
  const { push } = useToast()
  return useMemo(
    () => ({
      ok: (m: string) => push(m, 'ok'),
      err: (m: string) => push(m, 'error'),
    }),
    [push],
  )
}
