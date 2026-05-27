<!-- intent-skills:start -->
## Skill Loading

Before substantial work:
- Skill check: run `pnpm dlx @tanstack/intent@latest list`, or use skills already listed in context.
- Skill guidance: if one local skill clearly matches the task, run `pnpm dlx @tanstack/intent@latest load <package>#<skill>` and follow the returned `SKILL.md`.
- Monorepos: when working across packages, run the skill check from the workspace root and prefer the local skill for the package being changed.
- Multiple matches: prefer the most specific local skill for the package or concern you are changing; load additional skills only when the task spans multiple packages or concerns.
<!-- intent-skills:end -->

# Denia Web (TanStack Start)

Operator dashboard frontend for Denia, a Docker-free single-node PaaS. Lives in `web/`; the Rust backend control plane is the repo root. Keep frontend scope here unless asked otherwise.

## How this was scaffolded

Exact TanStack CLI command (run inside `web/`, then flattened into `web/`):

```bash
npx @tanstack/cli@latest create my-tanstack-app --agent --add-ons tanstack-query --package-manager pnpm
```

Follow-up TanStack Intent commands:

```bash
npx @tanstack/intent@latest install   # adds the Skill Loading block above
npx @tanstack/intent@latest list      # review installed intents / skills
```

The CLI generates a named subdirectory (`my-tanstack-app/`). Its contents were moved up into `web/` (inline project), the nested `.git` was removed (parent repo owns version control), and the `web/.gitkeep` placeholder was dropped.

## Stack

- **TanStack Start** (`@tanstack/react-start`) — full-stack React framework, SSR.
- **TanStack Router** (`@tanstack/react-router`, file-based routing in `src/routes/`).
- **TanStack Query** (`@tanstack/react-query`) — the one chosen CLI add-on. Demonstrated in `src/routes/demo/tanstack-query.tsx`; SSR integration via `@tanstack/react-router-ssr-query` and `src/integrations/tanstack-query/`.
- **TanStack CLI** (`@tanstack/cli`) — scaffolder (command above).
- **TanStack Intent** (`@tanstack/intent`) — skill loader; consult before architectural/library changes.
- **Effect** (`effect@beta`, effect-smol) — frontend logic/data layer beneath
  TanStack Query. See "Effect logic layer" below and `docs/adr/002`.
- React 19, TypeScript, Vite 8, Vitest (`@effect/vitest` for Effect tests).
- **Bundled by the CLI, not explicitly requested:** Tailwind CSS v4 (`@tailwindcss/vite`, `src/styles.css`) and `lucide-react` icons. Left in place because they are part of the canonical CTA scaffold; not extended. Remove later if a leaner base is wanted.

## Effect logic layer

Effect owns app logic; TanStack Query stays the cache/SSR/React-state layer and
runs Effect programs in its `queryFn`. Code lives in `src/effect/`:

- `config.ts` — `AppConfig` service (`Context.Service`): `baseUrl`, optional bearer `token` from env.
- `errors.ts` — `ApiError`, `DecodeError` (`Schema.TaggedErrorClass`).
- `schema.ts` — `Node` (`Schema.Class`) + `Nodes` array; validates the wire boundary.
- `api-client.ts` — `ApiClient` service over core `FetchHttpClient` (isomorphic: browser + SSR). `listNodes` decodes a fixture when no base URL is set, else `GET {baseUrl}/v1/nodes`.
- `runtime.ts` — one module-scope `ManagedRuntime` + `runQuery(effect)` helper bridged into Query.

Conventions (enforced by the `effect-ts` skill): no `any`/`as`, `Effect.fn`/`Effect.gen`,
services via `Context.Service` + `Layer`, schema-validated boundaries. The skill
reads a vendored source checkout at repo-root `.repos/effect` (gitignored,
bootstrapped by `pnpm install` -> `scripts/prepare-effect.sh`).

## Commands

```bash
pnpm dev        # vite dev --port 3000
pnpm build      # vite build
pnpm preview    # vite preview
pnpm test       # vitest run
pnpm typecheck  # tsc --noEmit
pnpm prepare    # bootstrap .repos/effect (runs on install)
```

Path alias: `#/*` -> `./src/*` (see `package.json` imports).

## Environment variables

Optional (client-exposed, must be `VITE_`-prefixed):

- `VITE_DENIA_API_URL` — control-plane base URL. Unset/empty -> `ApiClient.listNodes`
  serves a local fixture (offline demo). Set -> real `GET {baseUrl}/v1/nodes`.
- `VITE_DENIA_TOKEN` — **DEV-ONLY** bearer token sent as `Authorization: Bearer <token>`.
  Only honored when `import.meta.env.DEV` is true; stripped from production
  bundles, and `pnpm build` under `NODE_ENV=production` hard-fails if it is set
  (it would otherwise be embedded in the public SPA). Use runtime login in prod.

## Deployment

Served by the Rust binary, not a Node server (ADR-004). `vite.config.ts` runs
Start in **SPA mode** (`tanstackStart({ spa: { enabled: true } })`), so `pnpm build`
prerenders a static shell `dist/client/_shell.html` + hashed `assets/`. The Rust
crate embeds `web/dist/client` via `rust-embed` (`src/web.rs`) and serves it as a
router fallback after `/healthz` and `/v1`. Same origin as the API, no CORS, no
Node at runtime. SSR is dropped.

Build + run flow:

```bash
cd web && pnpm build        # emits dist/client/_shell.html + assets
cd .. && cargo run          # serves API + UI on DENIA_BIND_ADDR (default 127.0.0.1:7180)
```

A release `cargo build` needs `web/dist/client` present first (it is gitignored).

## Architectural decisions

- **Inline in `web/`**, not a nested subdir, so the frontend is a first-class part of the repo.
- **Single CLI add-on: tanstack-query.** No router/store/form/etc add-ons. Kept minimal/blank per brief.
- **File-based routing** (`mode: file-router` in `.cta.json`).
- **Examples kept**, not stripped: `index.tsx`, `about.tsx`, `demo/tanstack-query.tsx`, `Header/Footer/ThemeToggle`. The Query demo route is the required demonstration of TanStack Query; removing the canonical examples is an architectural edit deferred until there's real dashboard UI to replace them.
- Design context for the eventual dashboard lives in repo-root `PRODUCT.md` and `DESIGN.md` (mono-forward, dark-primary, "Stagecraft/Breakdown" pink+violet signal palette). The design system is applied in `src/styles.css` + components.
- **Effect under TanStack Query** for the logic/data layer (`docs/adr/002`): typed errors, `Context.Service`+`Layer` DI, `Schema`-validated boundary, one `ManagedRuntime` bridged via `runQuery`.

## Known gotchas

- `@tanstack/intent install` overwrote the CTA-generated `AGENTS.md`; this file is the rewrite. Scaffold docs survive in `web/README.md`.
- `@tanstack/react-query` ships **no** Intent skill, so `intent list` won't show Query guidance. Router, Start, and devtools skills are available; load them with `pnpm dlx @tanstack/intent@latest load <package>#<skill>`.
- Vite 8 + the devtools plugin must stay first in the `plugins` array (`vite.config.ts`).
- Many deps pinned to `latest` by the CLI; `pnpm-lock.yaml` holds the resolved versions.

## Next steps

1. Decide whether to trim CTA chrome (`about.tsx`, Header/Footer/ThemeToggle, Tailwind, lucide) for a truly blank base, or keep as the UI starting point.
2. Add real `ApiClient` methods (services, deployments, routes, secrets, metrics) against `/v1`; set `VITE_DENIA_API_URL`/`VITE_DENIA_TOKEN` to switch `listNodes` off the fixture.
3. Pick and document a deployment target.
4. Build real routes per `DESIGN.md` (services, deployments, routes, secrets, runtime metrics).
