# ADR-004: Embed the Web Console in the Service Binary

## Status

Proposed

## Date

2026-05-24

## Context

Denia ships an operator console (`web/`, TanStack Start + Router + Query +
Effect) alongside the Rust control plane. Running them as two separate processes
adds operational friction for a single-node, self-hosted PaaS. The goal is one
command and, ideally, one self-contained artifact: running the service also
serves the UI.

TanStack Start is SSR/full-stack and normally requires a Node server at runtime
(it emits `dist/server/server.js` and no static entry document). That conflicts
with shipping a single Rust binary and with not requiring Node on the host.

## Decision

Serve the console from the Rust binary as a static single-page app.

- **SPA mode, no SSR.** `web/vite.config.ts` enables `tanstackStart({ spa: { enabled: true } })`.
  `pnpm build` prerenders a static shell at `web/dist/client/_shell.html` plus
  hashed assets under `web/dist/client/assets/`.
- **Embed in the binary.** A new `src/web.rs` embeds `web/dist/client` via
  `rust-embed` (feature `mime-guess`). Release builds bake the assets into the
  binary; debug builds read them from disk.
- **Routing.** `src/app.rs` `build_router` keeps `/healthz` and the
  bearer-protected `nest("/v1", ...)`, then adds `.fallback(web::static_handler)`.
  The handler serves the matched embedded asset with its guessed MIME type;
  unmatched non-asset paths resolve to `_shell.html` (client-side routing);
  unmatched asset-looking paths (last segment has a `.`) return 404. The console
  is same-origin with `/v1`, so no CORS.
- **Build flow.** Two-step build, one run: `pnpm build` (in `web/`) then
  `cargo build`/`cargo run`. The single binary then serves both API and UI on
  `DENIA_BIND_ADDR` (default `127.0.0.1:7180`).

## Consequences

Easier:

- One process, one command; a single self-contained binary suitable for shipping.
- No Node runtime in production; UI and API share an origin and the same auth surface.

Harder:

- SSR is dropped (acceptable for an authenticated, client-rendered operator tool).
- Rebuilding the web requires recompiling Rust to refresh embedded assets.
- `web/dist` is gitignored, so a clean checkout must run `pnpm build` before a
  release `cargo build` (otherwise `rust-embed` has nothing to embed).

## Alternatives Considered

- **Keep SSR; Rust spawns Node and reverse-proxies.** Preserves SSR and server
  functions but requires Node at runtime and runs two processes. Rejected: defeats
  the single-binary goal for a single-node PaaS.
- **Serve `dist/client` from a disk directory** (`tower-http` `ServeDir`). No
  recompile on web change, but the directory must ship beside the binary.
  Rejected in favour of a self-contained binary; can revisit if web iteration
  speed matters more than packaging.

## References

- `docs/adr/001-initial-backend-architecture.md`
- `docs/adr/002-frontend-effect-logic-layer.md`
- `src/web.rs`, `src/app.rs`, `web/vite.config.ts`
- rust-embed: <https://github.com/pyrossh/rust-embed>
- TanStack Start SPA mode: <https://tanstack.com/start/latest>
