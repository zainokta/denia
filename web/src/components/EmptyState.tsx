import type { ReactNode } from 'react'

// Empty states earn their pixels: a reason and a next step, never a bare
// "No X." string. Icon is optional and decorative (aria-hidden).
export function EmptyState({
  icon,
  title,
  hint,
  action,
}: {
  readonly icon?: ReactNode
  readonly title: string
  readonly hint?: string
  readonly action?: ReactNode
}) {
  return (
    <div className="empty">
      {icon ? (
        <span className="empty-icon" aria-hidden="true">
          {icon}
        </span>
      ) : null}
      <p className="empty-title">{title}</p>
      {hint ? <p className="empty-hint">{hint}</p> : null}
      {action ? <div className="cluster" style={{ justifyContent: 'center' }}>{action}</div> : null}
    </div>
  )
}
