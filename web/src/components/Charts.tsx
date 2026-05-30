import { useId, useState } from 'react'
import { useResizeWidth } from '#/hooks/useResizeWidth'
import type { SemState } from '#/lib/status'

// Hand-rolled, zero-dependency SVG charts tuned to the Stagecraft/Breakdown
// system: flat, mono axes with tabular figures, signal-coloured strokes,
// no gradients or glow. Motion is limited to the gauge fill (CSS, reduced-
// motion gated). Charts read final state immediately for accessibility.

const STATE_VAR: Record<SemState, string> = {
  steady: 'var(--pink)',
  ok: 'var(--ok)',
  warn: 'var(--warn)',
  fault: 'var(--violet)',
  muted: 'var(--fg-muted)',
}

export function stateColor(state: SemState): string {
  return STATE_VAR[state]
}

// --- Sparkline -------------------------------------------------------------

interface SparklineProps {
  readonly values: ReadonlyArray<number>
  readonly width?: number
  readonly height?: number
  readonly color?: string
  readonly area?: boolean
  readonly className?: string
  readonly ariaLabel?: string
}

export function Sparkline({
  values,
  width = 120,
  height = 28,
  color = 'var(--pink)',
  area = true,
  className,
  ariaLabel,
}: SparklineProps) {
  const gradId = useId()
  if (values.length === 0) {
    return (
      <svg
        className={`spark ${className ?? ''}`}
        width={width}
        height={height}
        role="img"
        aria-label={ariaLabel ?? 'no data'}
      />
    )
  }
  const min = Math.min(...values)
  const max = Math.max(...values)
  const span = max - min || 1
  const n = values.length
  const pad = 1.5
  const x = (i: number) =>
    n === 1 ? width / 2 : pad + (i / (n - 1)) * (width - pad * 2)
  const y = (v: number) =>
    pad + (1 - (v - min) / span) * (height - pad * 2)
  const line = values.map((v, i) => `${x(i).toFixed(2)},${y(v).toFixed(2)}`)
  const linePath = `M${line.join(' L')}`
  const areaPath = `${linePath} L${x(n - 1).toFixed(2)},${height} L${x(0).toFixed(2)},${height} Z`

  return (
    <svg
      className={`spark ${className ?? ''}`}
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label={ariaLabel ?? `trend, latest ${values[n - 1]}`}
    >
      {area ? (
        <>
          <defs>
            <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity="0.22" />
              <stop offset="100%" stopColor={color} stopOpacity="0" />
            </linearGradient>
          </defs>
          <path d={areaPath} fill={`url(#${gradId})`} stroke="none" />
        </>
      ) : null}
      <path
        d={linePath}
        fill="none"
        stroke={color}
        strokeWidth={1.25}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  )
}

// --- Area / line time-series chart -----------------------------------------

export interface AreaSeries {
  readonly label: string
  readonly color: string
  readonly values: ReadonlyArray<number>
}

interface AreaChartProps {
  readonly series: ReadonlyArray<AreaSeries>
  readonly xLabels?: ReadonlyArray<string>
  readonly height?: number
  readonly yMax?: number
  readonly yFormat?: (v: number) => string
  readonly className?: string
}

const PAD = { t: 10, r: 12, b: 22, l: 46 }

export function AreaChart({
  series,
  xLabels,
  height = 200,
  yMax,
  yFormat = (v) => v.toFixed(0),
  className,
}: AreaChartProps) {
  const [ref, width] = useResizeWidth<HTMLDivElement>()
  const [hover, setHover] = useState<number | null>(null)
  const gradId = useId()

  const n = series.reduce((m, s) => Math.max(m, s.values.length), 0)
  const dataMax =
    yMax ??
    Math.max(
      1,
      ...series.flatMap((s) => (s.values.length ? [...s.values] : [0])),
    )
  const niceMax = roundUpNice(dataMax)

  const plotW = Math.max(0, width - PAD.l - PAD.r)
  const plotH = Math.max(0, height - PAD.t - PAD.b)
  const xAt = (i: number) =>
    n <= 1 ? PAD.l + plotW / 2 : PAD.l + (i / (n - 1)) * plotW
  const yAt = (v: number) =>
    PAD.t + plotH - (Math.min(v, niceMax) / niceMax) * plotH

  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => f * niceMax)

  const onMove = (clientX: number, rect: DOMRect) => {
    if (n <= 1) return setHover(0)
    const rel = clientX - rect.left - PAD.l
    const idx = Math.round((rel / plotW) * (n - 1))
    setHover(Math.max(0, Math.min(n - 1, idx)))
  }

  return (
    <div ref={ref} className={`relative ${className ?? ''}`} style={{ minHeight: height }}>
      {width > 0 ? (
        <svg
          className="chart"
          width={width}
          height={height}
          role="img"
          aria-label={`${series.map((s) => s.label).join(', ')} over time`}
          onPointerMove={(e) =>
            onMove(e.clientX, e.currentTarget.getBoundingClientRect())
          }
          onPointerLeave={() => setHover(null)}
        >
          <defs>
            {series.map((s, si) => (
              <linearGradient
                key={si}
                id={`${gradId}-${si}`}
                x1="0"
                y1="0"
                x2="0"
                y2="1"
              >
                <stop offset="0%" stopColor={s.color} stopOpacity="0.2" />
                <stop offset="100%" stopColor={s.color} stopOpacity="0" />
              </linearGradient>
            ))}
          </defs>

          {/* horizontal gridlines + y labels */}
          {ticks.map((t, i) => (
            <g key={i}>
              <line
                className="chart-grid-line"
                x1={PAD.l}
                x2={width - PAD.r}
                y1={yAt(t)}
                y2={yAt(t)}
              />
              <text className="chart-axis" x={PAD.l - 6} y={yAt(t) + 3} textAnchor="end">
                {yFormat(t)}
              </text>
            </g>
          ))}

          {/* series areas + lines */}
          {series.map((s, si) => {
            if (s.values.length === 0) return null
            const pts = s.values.map(
              (v, i) => `${xAt(i).toFixed(2)},${yAt(v).toFixed(2)}`,
            )
            const linePath = `M${pts.join(' L')}`
            const last = s.values.length - 1
            const areaPath = `${linePath} L${xAt(last).toFixed(2)},${(PAD.t + plotH).toFixed(2)} L${xAt(0).toFixed(2)},${(PAD.t + plotH).toFixed(2)} Z`
            return (
              <g key={si}>
                <path d={areaPath} fill={`url(#${gradId}-${si})`} className="chart-area" />
                <path d={linePath} className="chart-line" stroke={s.color} />
              </g>
            )
          })}

          {/* x labels: first / mid / last */}
          {xLabels && xLabels.length > 0
            ? [0, Math.floor((n - 1) / 2), n - 1]
                .filter((i, idx, arr) => arr.indexOf(i) === idx && xLabels[i])
                .map((i) => (
                  <text
                    key={i}
                    className="chart-axis"
                    x={clamp(xAt(i), PAD.l, width - PAD.r)}
                    y={height - 6}
                    textAnchor={i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle'}
                  >
                    {xLabels[i]}
                  </text>
                ))
            : null}

          {/* hover cursor + dots */}
          {hover !== null ? (
            <>
              <line
                className="chart-cursor"
                x1={xAt(hover)}
                x2={xAt(hover)}
                y1={PAD.t}
                y2={PAD.t + plotH}
              />
              {series.map((s, si) =>
                s.values[hover] !== undefined ? (
                  <circle
                    key={si}
                    className="chart-dot"
                    cx={xAt(hover)}
                    cy={yAt(s.values[hover])}
                    r={3}
                    stroke={s.color}
                  />
                ) : null,
              )}
            </>
          ) : null}
        </svg>
      ) : null}

      {/* tooltip */}
      {hover !== null && width > 0 ? (
        <div
          className="panel panel-pad"
          style={{
            position: 'absolute',
            top: 4,
            left: clamp(xAt(hover) + 8, PAD.l, Math.max(PAD.l, width - 150)),
            pointerEvents: 'none',
            padding: '0.4rem 0.55rem',
            fontSize: 'var(--text-label)',
            zIndex: 5,
          }}
        >
          {xLabels?.[hover] ? (
            <div className="text-faint" style={{ marginBottom: 2 }}>
              {xLabels[hover]}
            </div>
          ) : null}
          {series.map((s, si) => (
            <div key={si} className="cluster" style={{ gap: '0.4rem' }}>
              <span
                className="signal"
                style={{ background: s.color }}
                aria-hidden="true"
              />
              <span className="text-faint">{s.label}</span>
              <span className="tnum" style={{ marginLeft: 'auto' }}>
                {s.values[hover] !== undefined ? yFormat(s.values[hover]) : '—'}
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  )
}

// --- Radial gauge ----------------------------------------------------------

interface RadialGaugeProps {
  readonly value: number // 0..max
  readonly max?: number
  readonly size?: number
  readonly label: string
  readonly sublabel?: string
  readonly state?: SemState
}

const SWEEP = 270 // degrees
const START = -135 // degrees from top, clockwise

function polar(cx: number, cy: number, r: number, deg: number): [number, number] {
  const a = ((deg - 90) * Math.PI) / 180
  return [cx + r * Math.cos(a), cy + r * Math.sin(a)]
}

function arc(cx: number, cy: number, r: number, startDeg: number, endDeg: number): string {
  const [x1, y1] = polar(cx, cy, r, startDeg)
  const [x2, y2] = polar(cx, cy, r, endDeg)
  const large = Math.abs(endDeg - startDeg) > 180 ? 1 : 0
  return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${r} ${r} 0 ${large} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`
}

export function RadialGauge({
  value,
  max = 100,
  size = 132,
  label,
  sublabel,
  state = 'steady',
}: RadialGaugeProps) {
  const pct = Math.max(0, Math.min(1, max > 0 ? value / max : 0))
  const stroke = Math.round(size * 0.085)
  const r = (size - stroke) / 2 - 2
  const cx = size / 2
  const cy = size / 2
  const endDeg = START + SWEEP * pct
  return (
    <div className="relative inline-flex" style={{ width: size, height: size }}>
      <svg width={size} height={size} role="img" aria-label={`${label}: ${value.toFixed(0)} of ${max}`}>
        <path
          className="gauge-track"
          d={arc(cx, cy, r, START, START + SWEEP)}
          strokeWidth={stroke}
        />
        <path
          className="gauge-fill"
          d={arc(cx, cy, r, START, endDeg)}
          stroke={STATE_VAR[state]}
          strokeWidth={stroke}
          fill="none"
        />
      </svg>
      <div
        className="absolute inset-0 flex flex-col items-center justify-center text-center"
        style={{ paddingBottom: size * 0.08 }}
      >
        <span className="t-title tnum">{label}</span>
        {sublabel ? (
          <span className="kicker" style={{ marginTop: 2 }}>
            {sublabel}
          </span>
        ) : null}
      </div>
    </div>
  )
}

// --- Bar meter (inline usage) ----------------------------------------------

interface BarMeterProps {
  readonly value: number
  readonly max: number
  readonly state?: SemState
  readonly className?: string
}

export function BarMeter({ value, max, state = 'steady', className }: BarMeterProps) {
  const pct = Math.max(0, Math.min(100, max > 0 ? (value / max) * 100 : 0))
  const fillMod =
    state === 'fault' ? 'is-fault' : state === 'warn' ? 'is-warn' : ''
  return (
    <div
      className={`meter ${className ?? ''}`}
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <span className={`meter-fill ${fillMod}`} style={{ width: `${pct}%` }} />
    </div>
  )
}

// --- helpers ---------------------------------------------------------------

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v))
}

function roundUpNice(v: number): number {
  if (v <= 0) return 1
  const mag = Math.pow(10, Math.floor(Math.log10(v)))
  const norm = v / mag
  const step = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10
  return step * mag
}
