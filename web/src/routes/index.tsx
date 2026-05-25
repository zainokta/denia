import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/')({ component: App })

const surfaces: Array<[string, string, string]> = [
  ['services', 'deploy, inspect, restart workloads', 'steady'],
  ['routes', 'Denia-owned sockets, Traefik bridge', 'steady'],
  ['secrets', 'SOPS-referenced, never stored raw', 'steady'],
  ['runtime', 'cgroup v2 + procfs metrics', 'steady'],
]

function App() {
  return (
    <main className="page-wrap px-4 pb-12 pt-12">
      <section className="max-w-3xl">
        <p className="kicker mb-3">single-node control plane</p>
        <h1 className="mb-4 text-3xl font-semibold leading-tight tracking-tight text-[var(--fg)] sm:text-4xl">
          Run workloads without Docker. Watch the machine directly.
        </h1>
        <p className="prose-sans mb-7 text-[var(--fg-muted)]">
          Denia is a Docker-free PaaS with its own Linux runtime isolation. This
          console exposes the control-plane API as a fast operator surface: no
          magic, no hidden state, just what the node is doing.
        </p>
        <div className="flex flex-wrap gap-3">
          <a href="/demo/tanstack-query" className="btn btn-primary">
            View live data
          </a>
          <a href="/about" className="btn">
            About denia
          </a>
        </div>
      </section>

      <section className="mt-12">
        <p className="kicker mb-3">state legend</p>
        <div className="flex flex-wrap gap-x-6 gap-y-2 text-xs text-[var(--fg-muted)]">
          <span className="inline-flex items-center gap-2">
            <span className="signal signal-steady" /> Stagecraft &mdash; steady /
            healthy
          </span>
          <span className="inline-flex items-center gap-2">
            <span className="signal signal-fault" /> Breakdown &mdash; fault /
            attention
          </span>
          <span className="inline-flex items-center gap-2">
            <span className="signal signal-ok" /> ok
          </span>
          <span className="inline-flex items-center gap-2">
            <span className="signal signal-warn" /> warn
          </span>
        </div>
      </section>

      <section className="panel mt-8 overflow-hidden">
        <p className="kicker border-b border-[var(--border)] px-4 py-2.5">
          control surfaces
        </p>
        <dl className="m-0">
          {surfaces.map(([name, desc], i) => (
            <div
              key={name}
              className={`flex items-baseline gap-4 px-4 py-3 ${
                i > 0 ? 'border-t border-[var(--border)]' : ''
              }`}
            >
              <dt className="flex w-32 flex-shrink-0 items-center gap-2 text-sm font-semibold text-[var(--fg)]">
                <span className="signal signal-steady" />
                {name}
              </dt>
              <dd className="m-0 text-sm text-[var(--fg-muted)]">{desc}</dd>
            </div>
          ))}
        </dl>
      </section>

      <section className="mt-8">
        <p className="kicker mb-2">quick start</p>
        <ul className="m-0 list-none space-y-1.5 text-sm text-[var(--fg-muted)]">
          <li>
            edit <code>src/routes/index.tsx</code> for this overview
          </li>
          <li>
            wire the control-plane API in a Query client under{' '}
            <code>src/integrations/tanstack-query/</code>
          </li>
          <li>
            visual tokens live in <code>src/styles.css</code>; design law in
            repo-root <code>DESIGN.md</code>
          </li>
        </ul>
      </section>
    </main>
  )
}
