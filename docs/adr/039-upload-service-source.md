# ADR-039: Upload Service Source + `denia init`/`create` Workflow

- **Status**: Accepted
- **Date**: 2026-06-05

## Context

ADR-034 introduced `denia push`: pack the local working tree, upload it, and
build it on the node as a `DeploymentRequest::Upload`. But a deployment always
targets an existing service, and every service requires a `ServiceSource`, whose
only variants were `Git` and `ExternalImage` (`src/domain/service.rs`).

That left the push-first workflow with a chicken-and-egg problem: to deploy by
upload you must first create a service, but the service-creation surface forced
you to name a Git repo or an OCI image you do not have and will never use — the
Upload deploy path ignores `service.source` entirely
(`src/deploy/coordinator.rs`). The two existing entry points both papered over
it badly:

- The **web console** (`ServiceForm`) offered only Git / External Image, so an
  upload user had to invent a fake image reference.
- **`denia push --create`** fabricated a junk hosted-registry image ref as the
  placeholder source and refused to run unless a `control_domain` was configured
  on the node — neither of which an upload deploy needs.

The ADR-034 implementation plan explicitly anticipated "an upload/placeholder
source variant consistent with the existing variants" but deferred it.

## Decision

1. Add a first-class unit variant `ServiceSource::Upload` (serialized
   `{"type":"upload"}` via the existing `#[serde(tag = "type")]` enum). It
   carries no fields: the build context is supplied per-deploy by `denia push`,
   never stored on the service. `ServiceConfig::validate` accepts it with no
   checks. Jobs (`src/scheduler.rs`) reject it — a one-shot job has no way to
   supply a working-tree upload.

2. Add a CLI-first workflow driven by the `.denia` manifest:
   - `denia init` scaffolds a `.denia` template with the minimum required fields
     uncommented (`project`, `service`, `[create]` `port`) and optional ones
     commented, for the operator to edit.
   - `denia create` reads `.denia` (required) and creates the service with an
     `upload` source via a shared `create_service_from_manifest` helper. It
     resolves-or-creates the project and refuses to clobber an existing service.
     No `control_domain` dependency.
   - `denia push` deploys the working tree. `denia push --create` is retained as
     a thin alias that calls the same shared helper.

3. The web console gains a matching "Upload (deploy via CLI push)" radio that
   collects no source fields, so console-created services are consistent with
   the CLI path.

4. Add a `DeploymentRequest::Redeploy { service_id }` variant so the console
   "deploy" button works for upload-source services. The coordinator handles it
   by loading the service's current promoted deployment's stored artifact
   (`promoted_deployment` + `get_deployment_artifact`) and finalizing with it —
   no build, no pull. The console sends `Redeploy` only for upload services;
   Git/ExternalImage "deploy" keeps rebuilding/re-pulling from source so it picks
   up new code. Redeploy errors with `NoExistingArtifact` when the service has no
   promoted artifact yet (e.g. an upload service that has never been pushed).

## Consequences

- **Easier**: upload users create a service without inventing a fake source, on
  any node, with or without a control domain. The console, the CLI, and the docs
  all agree on the three source kinds.
- **Backward-compatible**: the variant is additive on a tagged enum, and
  `source` is stored inside the `config_json` blob — existing service rows
  decode unchanged, no migration.
- **Harder/constrained**: an Upload-source service is deployable only via
  `denia push` (a Git/ExternalImage deployment request against it returns
  `UnsupportedServiceSource`/`UnsupportedGitSource`). The console "redeploy"
  action returns a client error for upload services, since it cannot supply a
  working-tree context.

## Alternatives Considered

- **Make `source` optional (`Option<ServiceSource>`)**: larger blast radius —
  every consumer must handle `None`, plus a serde shape change. Rejected for a
  unit variant that the type system already enforces exhaustively.
- **Docs/CLI-only (keep the fabricated placeholder)**: leaves dead source data
  on every upload service and keeps the console confusing. Rejected.

## References

- ADR-034: Client-Driven Deploy via Working-Tree Upload
- ADR-017: Service CRUD API
- `src/domain/service.rs`, `src/cli/client/{create,init,push}.rs`,
  `web/src/components/ServiceForm.tsx`, `web/src/effect/schema.ts`
