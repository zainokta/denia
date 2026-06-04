import { useEffect, useMemo, useRef, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import {
  Boxes,
  Container,
  FolderGit2,
  Gauge,
  KeyRound,
  LayoutDashboard,
  Network,
  Search,
  SunMoon,
  Terminal,
  Timer,
  Users,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

interface Command {
  readonly id: string
  readonly label: string
  readonly group: string
  readonly icon: LucideIcon
  readonly keywords?: string
  readonly run: () => void
}

function toggleTheme() {
  const root = document.documentElement
  const next = root.classList.contains('light') ? 'dark' : 'light'
  root.classList.remove('light', 'dark')
  root.classList.add(next)
  root.setAttribute('data-theme', next)
  root.style.colorScheme = next
  try {
    window.localStorage.setItem('theme', next)
  } catch {
    /* storage may be unavailable */
  }
}

// ⌘K / Ctrl-K command palette. Keyboard-respecting (PRODUCT.md): every primary
// destination and the theme toggle are reachable without a mouse. Optional —
// the sidebar still drives navigation for pointer users.
export function CommandPalette() {
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const inputRef = useRef<HTMLInputElement | null>(null)

  const commands = useMemo<ReadonlyArray<Command>>(() => {
    const go = (to: string) => () => {
      setOpen(false)
      void navigate({ to })
    }
    return [
      { id: 'overview', label: 'Overview', group: 'Navigate', icon: LayoutDashboard, run: go('/') },
      { id: 'services', label: 'Services', group: 'Navigate', icon: Boxes, keywords: 'deploy workload', run: go('/services') },
      { id: 'ingress', label: 'Ingress routes', group: 'Navigate', icon: Network, keywords: 'domains tls', run: go('/ingress') },
      { id: 'registries', label: 'Registries', group: 'Navigate', icon: Container, keywords: 'image pull docker ghcr registry', run: go('/registries') },
      { id: 'observability', label: 'Observability', group: 'Navigate', icon: Gauge, keywords: 'metrics cpu memory', run: go('/observability') },
      { id: 'jobs', label: 'Jobs', group: 'Navigate', icon: Timer, keywords: 'cron schedule', run: go('/jobs') },
      { id: 'projects', label: 'Projects', group: 'Navigate', icon: FolderGit2, keywords: 'team registry', run: go('/projects') },
      { id: 'users', label: 'Users', group: 'Settings', icon: Users, run: go('/settings/users') },
      { id: 'tokens', label: 'API tokens', group: 'Settings', icon: KeyRound, run: go('/settings/tokens') },
      { id: 'sessions', label: 'Sessions', group: 'Settings', icon: Terminal, run: go('/settings/sessions') },
      { id: 'oci', label: 'OCI layer cache', group: 'Settings', icon: Boxes, keywords: 'gc garbage', run: go('/settings/oci-cache') },
      { id: 'hosted-registry', label: 'Hosted registry', group: 'Settings', icon: Container, keywords: 'oci push image repository', run: go('/settings/hosted-registry') },
      {
        id: 'theme',
        label: 'Toggle light / dark theme',
        group: 'Actions',
        icon: SunMoon,
        run: () => {
          setOpen(false)
          toggleTheme()
        },
      },
    ]
  }, [navigate])

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return commands
    return commands.filter((c) =>
      `${c.label} ${c.group} ${c.keywords ?? ''}`.toLowerCase().includes(q),
    )
  }, [commands, query])

  // Global open shortcut.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setOpen((o) => !o)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  useEffect(() => {
    if (open) {
      setQuery('')
      setActive(0)
      inputRef.current?.focus()
    }
  }, [open])

  useEffect(() => {
    setActive(0)
  }, [query])

  if (!open) return null

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') return setOpen(false)
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((i) => Math.min(filtered.length - 1, i + 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((i) => Math.max(0, i - 1))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      filtered[active]?.run()
    }
  }

  let lastGroup = ''

  return (
    <div
      className="cmdk-scrim"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) setOpen(false)
      }}
    >
      <div className="cmdk" role="dialog" aria-modal="true" aria-label="Command palette">
        <div className="cluster" style={{ gap: 0, alignItems: 'center', paddingLeft: '0.75rem' }}>
          <Search size={15} aria-hidden="true" style={{ color: 'var(--fg-muted)' }} />
          <input
            ref={inputRef}
            className="cmdk-input"
            placeholder="Go to or run…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            aria-label="Command palette query"
            role="combobox"
            aria-expanded="true"
            aria-controls="cmdk-list"
            aria-activedescendant={filtered[active] ? `cmdk-${filtered[active].id}` : undefined}
          />
        </div>
        <ul id="cmdk-list" className="cmdk-list" role="listbox">
          {filtered.length === 0 ? (
            <li className="cmdk-empty">No matches.</li>
          ) : (
            filtered.map((c, i) => {
              const showGroup = c.group !== lastGroup
              lastGroup = c.group
              const Icon = c.icon
              return (
                <li key={c.id}>
                  {showGroup ? <p className="kicker cmdk-group-label">{c.group}</p> : null}
                  <div
                    id={`cmdk-${c.id}`}
                    role="option"
                    aria-selected={i === active}
                    className={`cmdk-item ${i === active ? 'is-active' : ''}`}
                    onMouseEnter={() => setActive(i)}
                    onMouseDown={(e) => {
                      e.preventDefault()
                      c.run()
                    }}
                  >
                    <Icon size={15} aria-hidden="true" />
                    {c.label}
                  </div>
                </li>
              )
            })
          )}
        </ul>
      </div>
    </div>
  )
}
