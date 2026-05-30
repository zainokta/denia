// Shimmer placeholders for loads over ~300ms (no blocking spinners). Reserves
// space to avoid layout shift; the shimmer animation is reduced-motion gated
// globally in styles.css.

export function Skeleton({
  width,
  height = '1rem',
  radius = 4,
  className,
}: {
  readonly width?: number | string
  readonly height?: number | string
  readonly radius?: number
  readonly className?: string
}) {
  return (
    <span
      className={`skeleton ${className ?? ''}`}
      aria-hidden="true"
      style={{
        display: 'block',
        width: width ?? '100%',
        height,
        borderRadius: radius,
      }}
    />
  )
}

// A panel of stacked skeleton rows for list/table loading.
export function SkeletonRows({
  rows = 4,
  className,
}: {
  readonly rows?: number
  readonly className?: string
}) {
  return (
    <div
      className={`panel ${className ?? ''}`}
      role="status"
      aria-busy="true"
      aria-label="Loading"
    >
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="flex items-center gap-4"
          style={{
            padding: '0.7rem 0.9rem',
            borderTop: i > 0 ? '1px solid var(--border)' : undefined,
          }}
        >
          <Skeleton width={8} height={8} radius={9999} />
          <Skeleton width="40%" />
          <Skeleton width={64} className="ml-auto" />
        </div>
      ))}
    </div>
  )
}
