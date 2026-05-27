import { useId, type KeyboardEvent, type ReactNode } from 'react'

export interface TabDef {
  id: string
  label: string
}

interface TabsProps {
  tabs: ReadonlyArray<TabDef>
  active: string
  onChange: (id: string) => void
  children: (activeId: string) => ReactNode
}

export function Tabs({ tabs, active, onChange, children }: TabsProps) {
  const baseId = useId()
  const tabId = (id: string) => `${baseId}-tab-${id}`
  const panelId = (id: string) => `${baseId}-panel-${id}`

  const activeIndex = Math.max(
    0,
    tabs.findIndex((t) => t.id === active),
  )

  function select(index: number) {
    const wrapped = (index + tabs.length) % tabs.length
    const next = tabs[wrapped]
    if (!next) return
    onChange(next.id)
    const el = document.getElementById(tabId(next.id))
    if (el instanceof HTMLElement) el.focus()
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        event.preventDefault()
        select(activeIndex + 1)
        break
      case 'ArrowLeft':
      case 'ArrowUp':
        event.preventDefault()
        select(activeIndex - 1)
        break
      case 'Home':
        event.preventDefault()
        select(0)
        break
      case 'End':
        event.preventDefault()
        select(tabs.length - 1)
        break
      default:
        break
    }
  }

  return (
    <div>
      <div
        role="tablist"
        tabIndex={0}
        onKeyDown={onKeyDown}
        className="flex items-center gap-5 border-b border-[var(--border)]"
      >
        {tabs.map((tab) => {
          const isActive = tab.id === active
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              id={tabId(tab.id)}
              aria-selected={isActive}
              aria-controls={panelId(tab.id)}
              tabIndex={isActive ? 0 : -1}
              onClick={() => onChange(tab.id)}
              className={`kicker nav-link cursor-pointer bg-transparent border-0 px-0 py-2 -mb-px${
                isActive ? ' is-active' : ''
              }`}
            >
              {tab.label}
            </button>
          )
        })}
      </div>
      <div
        role="tabpanel"
        id={panelId(active)}
        aria-labelledby={tabId(active)}
        tabIndex={0}
        className="pt-5"
      >
        {children(active)}
      </div>
    </div>
  )
}
