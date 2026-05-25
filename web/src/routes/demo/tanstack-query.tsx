import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '../../effect/api-client'
import { runQuery } from '../../effect/runtime'

export const Route = createFileRoute('/demo/tanstack-query')({
  component: TanStackQueryDemo,
})

// Effect program: pull the ApiClient service from context, run its listNodes effect.
const listNodes = Effect.gen(function* () {
  const api = yield* ApiClient
  return yield* api.listNodes
})

function TanStackQueryDemo() {
  const { data, isFetching, isError } = useQuery({
    queryKey: ['nodes'],
    queryFn: () => runQuery(listNodes),
    initialData: [],
  })

  const status = isError ? 'fault' : isFetching ? 'warn' : 'steady'

  return (
    <main className="page-wrap px-4 py-12">
      <p className="kicker mb-3">live data &middot; tanstack query</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)]">
        Query-owned state
      </h1>
      <p className="prose-sans mb-7 text-[var(--fg-muted)]">
        A TanStack Query cache, hydrated through TanStack Start SSR. Color
        reports query state, never decoration.
      </p>

      <section className="panel overflow-hidden">
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-2.5">
          <p className="kicker m-0">queryKey: ["nodes"]</p>
          <span className="inline-flex items-center gap-2 text-xs text-[var(--fg-muted)]">
            <span className={`signal signal-${status}`} />
            {status}
          </span>
        </div>
        <ul className="m-0 list-none">
          {data.map((node, i) => (
            <li
              key={node.id}
              className={`flex items-center gap-4 px-4 py-3 text-sm ${
                i > 0 ? 'border-t border-[var(--border)]' : ''
              }`}
            >
              <span className="tnum w-8 text-[var(--fg-muted)]">
                {String(node.id).padStart(2, '0')}
              </span>
              <span className="signal signal-steady" />
              <span className="text-[var(--fg)]">{node.name}</span>
            </li>
          ))}
        </ul>
      </section>
    </main>
  )
}
