import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/about')({
  component: About,
})

function About() {
  return (
    <main className="page-wrap px-4 py-12">
      <p className="kicker mb-3">about</p>
      <h1 className="mb-4 text-2xl font-semibold tracking-tight text-[var(--fg)] sm:text-3xl">
        An operator console, not a stage.
      </h1>
      <p className="prose-sans mb-4 text-[var(--fg-muted)]">
        Denia runs workloads with its own Linux runtime isolation instead of
        Docker, on a single node. The control plane stores state in SQLite,
        references secrets through SOPS, and owns its own ingress sockets.
      </p>
      <p className="prose-sans text-[var(--fg-muted)]">
        This console reads that machine directly. Color is reserved: Stagecraft
        pink for steady state, Breakdown violet for faults. Everything else stays
        quiet so the signal reads at a glance.
      </p>
    </main>
  )
}
