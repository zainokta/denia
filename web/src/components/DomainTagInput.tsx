import { useRef, useState, type KeyboardEvent, type ClipboardEvent } from 'react'

interface DomainTagInputProps {
  id?: string
  value: ReadonlyArray<string>
  onChange: (next: string[]) => void
  ariaDescribedBy?: string
  placeholder?: string
}

const DOMAIN_LABEL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/i

function isValidDomain(raw: string): boolean {
  if (raw.length === 0 || raw.length > 253) return false
  if (raw.startsWith('.') || raw.endsWith('.')) return false
  const labels = raw.split('.')
  if (labels.length < 2) return false
  return labels.every((label) => DOMAIN_LABEL.test(label))
}

function tokenize(raw: string): string[] {
  return raw
    .split(/[\s,]+/)
    .map((d) => d.trim())
    .filter((d) => d.length > 0)
}

export function DomainTagInput({
  id,
  value,
  onChange,
  ariaDescribedBy,
  placeholder = 'add domain',
}: DomainTagInputProps) {
  const [draft, setDraft] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)

  const commit = (raw: string) => {
    const tokens = tokenize(raw)
    if (tokens.length === 0) return
    const seen = new Set(value)
    const next = [...value]
    for (const t of tokens) {
      if (!seen.has(t)) {
        seen.add(t)
        next.push(t)
      }
    }
    if (next.length !== value.length) onChange(next)
    setDraft('')
  }

  const removeAt = (index: number) => {
    const next = value.filter((_, i) => i !== index)
    onChange(next)
  }

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' || e.key === ',' || e.key === ' ') {
      if (draft.trim().length > 0) {
        e.preventDefault()
        commit(draft)
      }
      return
    }
    if (e.key === 'Backspace' && draft.length === 0 && value.length > 0) {
      e.preventDefault()
      removeAt(value.length - 1)
    }
  }

  const onPaste = (e: ClipboardEvent<HTMLInputElement>) => {
    const text = e.clipboardData.getData('text')
    if (/[\s,]/.test(text)) {
      e.preventDefault()
      commit(draft + text)
    }
  }

  const onBlur = () => {
    if (draft.trim().length > 0) commit(draft)
  }

  const focusInput = () => inputRef.current?.focus()

  return (
    <div
      className="field-input field-chips"
      onClick={focusInput}
      role="presentation"
    >
      {value.map((domain, i) => {
        const invalid = !isValidDomain(domain)
        return (
          <span
            key={`${domain}-${i}`}
            className={`field-chip${invalid ? ' is-invalid' : ''}`}
            title={invalid ? 'invalid hostname' : domain}
          >
            {domain}
            <button
              type="button"
              aria-label={`remove ${domain}`}
              onClick={(e) => {
                e.stopPropagation()
                removeAt(i)
              }}
            >
              ×
            </button>
          </span>
        )
      })}
      <input
        ref={inputRef}
        id={id}
        type="text"
        className="field-chip-input"
        placeholder={value.length === 0 ? placeholder : ''}
        aria-describedby={ariaDescribedBy}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
        onBlur={onBlur}
      />
    </div>
  )
}
