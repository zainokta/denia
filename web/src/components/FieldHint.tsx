import { Info } from 'lucide-react'

interface FieldHintProps {
  id: string
  label: string
  children: React.ReactNode
}

export function FieldHint({ id, label, children }: FieldHintProps) {
  return (
    <span className="hint">
      <button
        type="button"
        className="hint-trigger"
        aria-label={label}
        aria-describedby={id}
        tabIndex={0}
      >
        <Info size={12} strokeWidth={1.75} aria-hidden="true" />
      </button>
      <span role="tooltip" id={id} className="hint-popup">
        {children}
      </span>
    </span>
  )
}
