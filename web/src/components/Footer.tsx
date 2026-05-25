export default function Footer() {
  const year = new Date().getFullYear()

  return (
    <footer className="mt-16 border-t border-[var(--border)] px-4 py-8 text-[var(--fg-muted)]">
      <div className="page-wrap flex flex-col items-start justify-between gap-2 text-xs sm:flex-row sm:items-center">
        <p className="m-0">
          <span className="tnum">{year}</span> denia. single-node control plane.
        </p>
        <p className="kicker m-0">Built with TanStack Start</p>
      </div>
    </footer>
  )
}
