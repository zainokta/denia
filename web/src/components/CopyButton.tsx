import { Check, Copy } from 'lucide-react'
import { useCallback, useRef, useState } from 'react'

// Copies text to the clipboard with transient confirmation. Used for tokens,
// ids, challenge values. Falls back silently if the clipboard API is absent.
export function CopyButton({
  value,
  label = 'Copy',
  className,
}: {
  readonly value: string
  readonly label?: string
  readonly className?: string
}) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const onCopy = useCallback(() => {
    const done = () => {
      setCopied(true)
      if (timer.current) clearTimeout(timer.current)
      timer.current = setTimeout(() => setCopied(false), 1500)
    }
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(value).then(done, () => undefined)
    }
  }, [value])

  return (
    <button
      type="button"
      className={`btn btn-icon ${className ?? ''}`}
      onClick={onCopy}
      aria-label={copied ? 'Copied' : label}
      title={copied ? 'Copied' : label}
    >
      {copied ? (
        <Check size={14} aria-hidden="true" style={{ color: 'var(--ok)' }} />
      ) : (
        <Copy size={14} aria-hidden="true" />
      )}
    </button>
  )
}
