import { useEffect, useRef, useState } from 'react'

// Destructive actions confirm inline (a popover anchored to the trigger), not
// in a modal. No-Surprises principle: the confirm step is explicit, danger-
// styled, and dismissable by Escape or clicking away.
export function ConfirmButton({
  label,
  confirmLabel = 'Confirm',
  message,
  onConfirm,
  busy = false,
  disabled = false,
  className = 'btn btn-danger',
  align = 'left',
}: {
  readonly label: React.ReactNode
  readonly confirmLabel?: string
  readonly message: string
  readonly onConfirm: () => void
  readonly busy?: boolean
  readonly disabled?: boolean
  readonly className?: string
  readonly align?: 'left' | 'right'
}) {
  const [open, setOpen] = useState(false)
  const wrapRef = useRef<HTMLSpanElement | null>(null)
  const confirmRef = useRef<HTMLButtonElement | null>(null)

  useEffect(() => {
    if (!open) return
    confirmRef.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    const onClick = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node))
        setOpen(false)
    }
    window.addEventListener('keydown', onKey)
    window.addEventListener('mousedown', onClick)
    return () => {
      window.removeEventListener('keydown', onKey)
      window.removeEventListener('mousedown', onClick)
    }
  }, [open])

  return (
    <span ref={wrapRef} style={{ position: 'relative', display: 'inline-flex' }}>
      <button
        type="button"
        className={className}
        disabled={disabled || busy}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        {busy ? <span className="spin" aria-hidden="true" /> : null}
        {label}
      </button>
      {open ? (
        <div
          className="confirm-pop"
          role="dialog"
          aria-label={message}
          style={{ top: 'calc(100% + 6px)', [align]: 0 }}
        >
          <p className="text-faint" style={{ marginBottom: 10, lineHeight: 1.5 }}>
            {message}
          </p>
          <div className="cluster" style={{ justifyContent: 'flex-end' }}>
            <button type="button" className="btn" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              ref={confirmRef}
              type="button"
              className="btn btn-danger"
              onClick={() => {
                setOpen(false)
                onConfirm()
              }}
            >
              {confirmLabel}
            </button>
          </div>
        </div>
      ) : null}
    </span>
  )
}
