export default function Footer() {
  const year = new Date().getFullYear()

  return (
    <footer className="mt-16 border-t border-[var(--border)] px-4 py-6 text-[var(--fg-muted)]">
      <div className="page-wrap text-xs">
        <p className="m-0">
          <span className="tnum">{year}</span> denia control plane
        </p>
      </div>
    </footer>
  )
}
