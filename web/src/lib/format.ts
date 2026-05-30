// Shared formatting helpers. Mono-forward UI: all numerics render with tabular
// figures (see `<Num>` / `.tnum`), so these return plain strings and the call
// site supplies the tabular class.

const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const

export function formatBytes(bytes: number, digits = 1): string {
  if (!Number.isFinite(bytes)) return '—'
  if (bytes < 1024) return `${Math.round(bytes)} B`
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(digits)} ${BYTE_UNITS[unit]}`
}

export function formatPercent(value: number, digits = 1): string {
  if (!Number.isFinite(value)) return '—'
  return `${value.toFixed(digits)}%`
}

export function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return '—'
  return value.toLocaleString('en-US')
}

// Compact duration from seconds: 45s, 12m 03s, 2h 05m, 3d 4h.
export function formatDuration(totalSeconds: number): string {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return '—'
  const s = Math.floor(totalSeconds % 60)
  const m = Math.floor((totalSeconds / 60) % 60)
  const h = Math.floor((totalSeconds / 3600) % 24)
  const d = Math.floor(totalSeconds / 86400)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${String(m).padStart(2, '0')}m`
  if (m > 0) return `${m}m ${String(s).padStart(2, '0')}s`
  return `${s}s`
}

export function formatMillis(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (ms < 1000) return `${Math.round(ms)}ms`
  return formatDuration(ms / 1000)
}

function parseDate(iso: string): number | null {
  const t = Date.parse(iso)
  return Number.isNaN(t) ? null : t
}

// Relative time vs a caller-supplied `now` (ms). Caller owns the clock so the
// function stays pure and testable; pass Date.now() at the call site.
export function formatRelative(iso: string, now: number): string {
  const t = parseDate(iso)
  if (t === null) return iso
  const deltaS = Math.round((now - t) / 1000)
  const abs = Math.abs(deltaS)
  const suffix = deltaS >= 0 ? 'ago' : 'from now'
  if (abs < 5) return 'just now'
  if (abs < 60) return `${abs}s ${suffix}`
  if (abs < 3600) return `${Math.floor(abs / 60)}m ${suffix}`
  if (abs < 86400) return `${Math.floor(abs / 3600)}h ${suffix}`
  return `${Math.floor(abs / 86400)}d ${suffix}`
}

// Stable absolute timestamp for tooltips / detail rows (local time, 24h).
export function formatDateTime(iso: string): string {
  const t = parseDate(iso)
  if (t === null) return iso
  return new Date(t).toLocaleString('en-US', {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  })
}

export function formatClock(iso: string): string {
  const t = parseDate(iso)
  if (t === null) return iso
  return new Date(t).toLocaleTimeString('en-US', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  })
}

// Short id for display: first segment of a UUID.
export function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id
}
