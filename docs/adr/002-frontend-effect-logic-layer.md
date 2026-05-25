# ADR-002: Frontend Effect Logic Layer

## Status

Proposed

## Date

2026-05-24

## Context

Denia gained an operator console under `web/`, a TanStack Start app (TanStack
Router + TanStack Query, React 19, TypeScript). The console will talk to the
control-plane `/v1` API: typed requests, bearer-token auth, response validation,
and predictable error handling matter, because the operator must trust what the
UI reports about the machine.

The scaffold fetched data with plain Promise functions and untyped JSON. That
gives no typed error channel, no dependency injection for the API client or
config, and no validation at the network boundary. We want the frontend logic
layer to match the backend's discipline of typed errors and explicit boundaries.

## Decision

Adopt **Effect** (`effect@beta`, the effect-smol line, currently
`4.0.0-beta.70`) as the `web/` frontend logic/data layer, sitting **beneath**
TanStack Query.

- TanStack Query remains the cache, SSR-hydration, and React-state layer. Its
  `queryFn`/`mutationFn` run Effect programs through one module-scope
  `ManagedRuntime` via a `runQuery` helper (`runtime.runPromise`).
- Application dependencies are Effect services defined with `Context.Service`
  and wired with `Layer`: `AppConfig` (base URL + optional bearer token from
  `import.meta.env`) and `ApiClient` (wraps the isomorphic `FetchHttpClient`
  from `effect/unstable/http`).
- Wire payloads are validated with `Schema` at the boundary; failures surface as
  typed errors (`ApiError`, `DecodeError`) defined via `Schema.TaggedErrorClass`.
- No `any`, no `as`; business logic uses `Effect.fn`/`Effect.gen`. This follows
  the repo `effect-ts` skill, which reads a vendored Effect source checkout at
  `.repos/effect`. That checkout is bootstrapped by clone + `.gitignore` + a
  `prepare` script (`web/scripts/prepare-effect.sh`); it is never committed.
- The browser HTTP layer uses core `FetchHttpClient` (works in both the browser
  and the SSR server) rather than `@effect/platform-browser`, avoiding an extra
  beta dependency and version-drift risk.

The first concrete slice: `ApiClient.listNodes`, demonstrated on the
`/demo/tanstack-query` route. It decodes a static fixture when no API base URL is
configured and performs a real `GET /v1/nodes` once `VITE_DENIA_API_URL` is set.

## Consequences

Easier:

- Typed end-to-end errors; validation at the network boundary.
- Dependency injection and testability: services are swapped via `Layer`, tests
  use `@effect/vitest` with stub layers.
- A clear seam for the real `/v1` client without touching React components.

Harder:

- Effect (and effect-smol specifically) has a learning curve; contributors must
  follow the `effect-ts` skill conventions.
- A beta dependency: API surface may shift before a stable release.
- Local/CI setup must bootstrap `.repos/effect` (the `prepare` script handles the
  common case).

## Alternatives Considered

- **Replace TanStack Query with an Effect-native binding (`@effect/atom`).**
  More idiomatic Effect, but discards the scaffold's Query + SSR wiring and the
  requested Query demonstration. Rejected: too much churn for no clear gain on a
  single-node console.
- **Effect and Query side by side** (Query for reads, Effect for imperative
  actions). Rejected: blurred ownership of state.
- **Keep plain Promises (status quo).** Rejected: no typed errors, no DI, no
  boundary validation, which is exactly what we want for control-plane calls.

## References

- `docs/adr/001-initial-backend-architecture.md`
- Effect: <https://effect.website/>
- effect-smol source: <https://github.com/Effect-TS/effect-smol>
- `web/AGENTS.md` (frontend stack, env vars, prepare step)
- TanStack Query: <https://tanstack.com/query/latest>
