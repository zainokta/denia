import type { ReactNode } from 'react'

// Tabular-figure wrapper. The Aligned-Number Rule: every metric, id, port, and
// duration renders with tabular figures so columns line up without padding.
export function Num({
  children,
  className,
  title,
}: {
  readonly children: ReactNode
  readonly className?: string
  readonly title?: string
}) {
  return (
    <span className={`tnum ${className ?? ''}`} title={title}>
      {children}
    </span>
  )
}
