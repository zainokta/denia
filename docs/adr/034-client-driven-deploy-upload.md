# ADR-034: Client-Driven Deploy via Working-Tree Upload

- Status: Accepted
- Date: 2026-06-03
- Supersedes: ADR-030 (deploy mechanism)
- Related: ADR-003, ADR-011, ADR-015, ADR-017, ADR-024, ADR-025, ADR-031, ADR-033

## Context

Operators want a Vercel-style local deploy loop: authenticate once, keep a
committed `.denia` project file, and run `denia push` from a project directory
to deploy the current working tree. The client scaffolding for this already
landed with the service console (ADR-033): the `.denia` manifest, named
`client.toml` profiles, and a bearer `ClientApi`.

ADR-030 designed `denia auth` + `denia push` but bound `push` to a **Git**
deploy: it required the local `HEAD` to match an upstream remote branch and let
the server clone that remote and build it. ADR-030 explicitly deferred
"local working tree snapshot upload" because Denia had "no source upload API."

That deferral is the friction: it forces a git remote the Denia node can reach,
a commit-and-push before every deploy, and excludes uncommitted edits. Denia
already builds Dockerfiles server-side from a confined directory via BuildKit
(`build_from_git_checkout` + `confine_under`), so the only missing piece is a
way to stage a client-supplied build context on the node.

## Decision

`denia push` packs the working tree, uploads it, and the server builds the
uploaded context with the existing BuildKit path. This supersedes ADR-030's
Git-based deploy mechanism. The `denia auth` token-minting flow from ADR-030 is
retained unchanged.

- **Auth:** `denia auth` prompts for URL, username, and password; calls
  `/v1/auth/login`; mints a named long-lived token via `/v1/api-tokens`; stores
  only `{url, token}` in `client.toml` with owner-only permissions; verifies via
  `/v1/me`. Password and session token are never persisted or logged.
- **Context:** `denia push` packs the **working tree** — tracked + untracked
  files honoring `.gitignore` and `.dockerignore`, with the Dockerfile always
  included — into a `tar.zst`. No commit or git remote is required.
- **Upload:** `POST /v1/services/{service_id}/uploads` (project Operator)
  streams the archive to `data_dir/uploads/<uuidv7>/`, enforcing a body-size cap
  and a hardened extraction that rejects absolute paths, `..` traversal,
  escaping symlinks/hardlinks, and special files, and caps uncompressed size and
  entry count. Returns an `upload_id` with a TTL.
- **Build + deploy:** `POST /v1/deployments` gains an `upload` source carrying
  the `upload_id`, mapped to a new `ArtifactSource::UploadedContext`. The
  acquirer builds it with the same `buildctl … --output type=oci` invocation as
  the git path, over the staged directory instead of a checkout. The staged
  upload is deleted after the build. Async deploy + SSE logs (ADR-024) are
  reused; the CLI tails them to completion.
- **Manifest:** the `.denia` manifest gains optional `dockerfile` (default
  `Dockerfile`), `context` (default `.`), and a `[create]` block (port, optional
  health path) consumed only by `denia push --create`. Existing 2-field
  manifests remain valid.
- **Service creation:** `denia push` requires an existing service by default;
  `--create` creates the project/service from `[create]` defaults.
- **Builds are Dockerfile-only** in this version; buildpacks/Nixpacks are out of
  scope. The cross-platform client/server command split from ADR-030 is not
  adopted here and remains future work.

## Consequences

- Easier: deploy exactly what is on disk, including uncommitted edits, with no
  git remote reachable by the node and no pre-deploy commit.
- Easier: reuses the existing BuildKit, async-deploy, and SSE-log subsystems;
  only an upload endpoint and one artifact-source variant are new.
- Harder: untrusted tarball extraction is a new host-root trust surface that
  must be hardened (path traversal, symlink escape, zip-bomb) and tested.
- Harder: each push uploads a full context (no incremental upload in v1).
- Constraint: the new client commands ship in the existing unified `denia`
  binary; building a Linux-runtime-free client for macOS/Windows still needs the
  ADR-030 command split, deferred to its own ADR.

## Alternatives Considered

- **Git-based deploy (ADR-030).** Rejected as the primary mechanism: requires a
  node-reachable git remote and a commit per deploy, and cannot deploy the
  working tree. Retained conceptually only for the auth flow.
- **Client builds the image and pushes to the hosted registry (ADR-031).**
  Rejected for v1: requires a local Docker/BuildKit toolchain on every developer
  machine, moving build cost and reproducibility off the node.
- **Fold the upload into the deploy request as one streaming POST.** Rejected:
  separating upload from deploy keeps the deploy request small and lets the SSE
  log stream attach by deployment id, matching ADR-024.
- **Deploy the committed `HEAD` instead of the working tree.** Rejected: the
  goal is a Vercel-style "deploy what I see" loop; honoring `.gitignore` +
  `.dockerignore` already prevents sweeping in junk.

## References

- [Spec: client-driven deploy via working-tree upload](../superpowers/specs/2026-06-03-denia-push-working-tree-deploy-design.md)
- ADR-030 (cross-platform client CLI — superseded deploy mechanism)
- ADR-024 (async deployments with per-deployment log stream)
- ADR-017 (service CRUD API)
- ADR-011 / ADR-015 (in-process OCI acquisition / streaming layer staging)
- ADR-031 (hosted OCI registry)
