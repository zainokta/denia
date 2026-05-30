import { AlertTriangle } from 'lucide-react'

// Surfaces an API error with the message the backend returned plus a recovery
// path. role="alert" so screen readers announce it. Breakdown-violet framed.
export function ErrorPanel({
  title = 'Something failed',
  message,
  onRetry,
}: {
  readonly title?: string
  readonly message: string
  readonly onRetry?: () => void
}) {
  return (
    <div
      className="panel panel-pad"
      role="alert"
      style={{
        borderColor: 'color-mix(in oklab, var(--violet) 45%, var(--border))',
      }}
    >
      <div className="cluster" style={{ alignItems: 'flex-start' }}>
        <AlertTriangle
          size={16}
          aria-hidden="true"
          style={{ color: 'var(--violet)', marginTop: 2 }}
        />
        <div style={{ minWidth: 0 }}>
          <p className="t-title" style={{ fontSize: 'var(--text-ui)' }}>
            {title}
          </p>
          <p className="text-faint" style={{ marginTop: 4, wordBreak: 'break-word' }}>
            {message}
          </p>
          {onRetry ? (
            <button type="button" className="btn" style={{ marginTop: 10 }} onClick={onRetry}>
              Retry
            </button>
          ) : null}
        </div>
      </div>
    </div>
  )
}

// Compact inline error for form fields / row-level failures.
export function InlineError({ message }: { readonly message: string }) {
  return (
    <p className="field-error" role="alert">
      {message}
    </p>
  )
}

// Maps an unknown error to a display string, preferring the API message.
export function errorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'message' in error) {
    const m = (error as { message: unknown }).message
    if (typeof m === 'string') return m
  }
  return error instanceof Error ? error.message : String(error)
}
