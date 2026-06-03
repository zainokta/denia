# Spec: Client-Driven Deploy via Working-Tree Upload (`denia auth` + `denia push`)

- Date: 2026-06-03
- Status: Approved (brainstorm), pending spec review
- Supersedes the deploy mechanism of ADR-030 (Cross-Platform Client CLI)
- New ADR: ADR-035 (Client-Driven Deploy via Working-Tree Upload)
- Related: ADR-003, ADR-011, ADR-015, ADR-017, ADR-024, ADR-025, ADR-031, ADR-033

## 1. Context

Denia ships a single `denia` binary that runs the daemon and operator host
commands. Client scaffolding already landed with the service console (ADR-033):

- `.denia` manifest (`src/cli/client/manifest.rs`) — currently `project` +
  `service` only.
- Client profile store (`src/cli/client/profile.rs`) — named `{url, token}`
  profiles in `client.toml`, **read-only** today (`load_from`, no writer).
- `ClientApi` HTTP client (`src/cli/client/http.rs`) — bearer `/v1` calls.
- `denia console` is the only client subcommand wired into `Commands`.

What is missing is a Vercel-style deploy loop: authenticate once, keep a
`.denia` project file, and `denia push` the current branch. ADR-030 (unmerged,
on branch `feat/cross-platform-client-cli`) designed this as a **Git-based**
deploy (require committed `HEAD == upstream`, server clones the git remote and
builds) and explicitly deferred working-tree snapshot upload to future work.

This spec takes that deferred path: **the client packs the working tree and
uploads it; the server builds the uploaded context with the existing BuildKit
path.** It supersedes ADR-030's deploy mechanism. The `denia auth` flow is kept
as ADR-030 specified it (it already matches our brainstorm).

### Server facts this design reuses

- `POST /v1/auth/login` → session token; `GET /v1/me` → principal.
- `POST /v1/api-tokens {name}` → long-lived token (returned once); `DELETE
  /v1/api-tokens/{id}` revoke.
- `POST /v1/deployments` is async: returns `202 Accepted` + a `Deployment`, and
  drives an SSE per-deployment log stream (ADR-024).
- Artifact build today: `ArtifactAcquirer::build_from_git_checkout` clones a git
  repo, calls `confine_under` to bind context/dockerfile paths inside the
  checkout, then runs `buildctl build --frontend dockerfile.v0 --local
  context=<dir> --local dockerfile=<dir> --output type=oci,dest=<artifact_dir>`.
  The uploaded-context build reuses this verbatim, swapping the git checkout for
  a server-staged extracted directory.
- `DeploymentRequest` is a serde-tagged enum (`source`): `git`, `external_image`.
- `ArtifactSource` is a serde-tagged enum (`type`): `build_kit`,
  `external_registry`.

## 2. Goals / Non-Goals

### Goals

- `denia auth`: interactive login → mint a long-lived API token → write a
  `client.toml` profile (owner-only perms). Password and session never stored.
- `denia push`: pack the working tree (Vercel-style), upload it, trigger a
  server-side BuildKit build of the uploaded Dockerfile, deploy, and stream
  deploy logs to the terminal.
- Extend `.denia` with optional build config, backward compatible with the
  existing 2-field manifest.
- A hardened server upload endpoint + a new `UploadedContext` artifact source.

### Non-Goals (YAGNI / explicitly deferred)

- Buildpacks / Nixpacks ("or equal" → **Dockerfile only** in v1).
- Cross-platform client/server **command split** (`denia server …`) and
  client-only build profiles from ADR-030. Out of scope here; the new client
  commands are added to the existing unified binary as `denia console` was.
  Tracked as future work (its own ADR) because the new client code (auth, push,
  pack) carries no Linux-only dependencies and can be split out later.
- Incremental / cached / diffed uploads. Each push uploads a full context.
- Multi-node build farm. Single-node only.
- Auto domain / TLS setup from the CLI.

## 3. Decisions (resolved in brainstorm)

| Fork | Decision |
|------|----------|
| Build location | **Server builds from uploaded context** (new upload endpoint + `UploadedContext` source). |
| Context packed | **Working tree** — tracked + untracked, honoring `.gitignore` + `.dockerignore`; Dockerfile always included. |
| Auth | **Login (user/pass) → auto-mint long-lived API token**; store token only. |
| Service create | **Opt-in `--create`** (default: require existing service). |
| Manifest | **Extend `.denia`** with optional build config; Dockerfile required, no buildpacks. |
| Upload transport | **Separate** upload → deploy (two calls), not one combined POST — keeps deploy JSON small and lets the SSE log stream attach by deployment id. |

## 4. Component Design

### 4.1 Client config writer — `src/cli/client/profile.rs`

`ClientConfig` is read-only today. Add:

- `upsert_profile(&mut self, name: &str, profile: Profile)` — insert/replace.
- `set_active(&mut self, name: &str)`.
- `save_to(&self, path: &Path)` — serialize TOML, write atomically
  (`tmp` + rename) with `0600` where the platform supports it.

No change to the existing read path or `Profile` shape (`url`, `token`).

### 4.2 `denia auth` — `src/cli/client/auth.rs` (new), `Commands::Auth`

`AuthArgs`: `--url <URL>`, `--username <USER>`, `--profile <NAME>` (default:
host of the URL), `--token-name <NAME>` (default `denia-cli@<hostname>`).

Flow:
1. Resolve URL (flag or prompt). Normalize (trim trailing `/`). Probe
   `GET /healthz`; fail clearly if unreachable.
2. Prompt username (flag or prompt) + password (no-echo via `rpassword`).
3. `POST /v1/auth/login` → session token (memory only).
4. `POST /v1/api-tokens {name: token-name}` with session bearer → long-lived
   token (server returns `token` once).
5. `GET /v1/me` with the new token to verify it works.
6. `upsert_profile` + `set_active` + `save_to(client.toml)`.

Security: password and session token are never written to disk or logs. Only
the long-lived API token is persisted; it is revocable from the web console.

New `ClientApi` methods: `login`, `create_api_token`, `me`.

### 4.3 `.denia` manifest — `src/cli/client/manifest.rs`

Extend `DeniaManifest` (still TOML, serde `Deserialize` + add `Serialize` for
`--create` write-back):

```toml
project = "default"        # required (existing)
service = "api"            # required (existing)

dockerfile = "Dockerfile"  # optional, default "Dockerfile"
context    = "."           # optional, default ".", relative to the .denia dir

[create]                   # optional; consumed only by `denia push --create`
port = 8080                # required when [create] used / created
health_path = "/healthz"   # optional
```

- New fields are `#[serde(default)]` so existing 2-field manifests parse
  unchanged.
- `dockerfile`/`context` resolve relative to the directory containing `.denia`.
- `[create]` is only read when `--create` is passed; `push` writes/updates the
  manifest after a successful `--create`.

### 4.4 Working-tree packer — `src/cli/client/pack.rs` (new)

Input: context root (manifest `context`, resolved against `--path`),
`dockerfile` relative path.

File selection:
- **Git repo present** (detect `.git`): seed the file set from
  `git ls-files --cached --others --exclude-standard` run in the context root —
  this yields tracked + untracked files with `.gitignore` already applied. Then
  apply `.dockerignore` filtering on top.
- **No git**: walk the directory honoring `.dockerignore` only.
- The Dockerfile is **always** included even if a `.gitignore`/`.dockerignore`
  rule would exclude it (Docker build semantics).

Packing:
- Stream selected files into a `tar` archive, then `zstd`-compress to a temp
  file. Deterministic ordering (sorted paths) for reproducibility.
- Client-side guards: max total uncompressed bytes and max file count; fail with
  a clear message and a hint to tighten `.dockerignore` if exceeded.

### 4.5 Server upload endpoint + new artifact source

Domain additions:
- `DeploymentRequest::Upload { service_id, upload_id, dockerfile_path,
  context_path }` (serde tag `upload`). `service_id()` extended.
- `ArtifactSource::UploadedContext { staged_dir, dockerfile_path, context_path }`
  (serde tag `uploaded_context`).

Upload endpoint — `POST /v1/services/{service_id}/uploads`:
- Mounted under `/v1`, requires project **Operator** (same as deploy).
- Body: a `tar.zst` stream (`Content-Type: application/zstd`).
- Stream body to `data_dir/uploads/<uuidv7>/context.tar.zst` with a **hard
  body-size cap** (configurable; `413 Payload Too Large` if exceeded — abort and
  delete the partial file).
- **Hardened extraction** to `data_dir/uploads/<id>/context/`:
  - reject absolute member paths and any `..` component;
  - reject symlink / hardlink members that resolve outside the extraction root;
  - reject device / fifo / special entries;
  - cap total uncompressed size and entry count (zip-bomb guard);
  - on any violation: delete the upload dir, return `400`.
- Returns `{ upload_id, expires_at }`. Upload tracked with a TTL.
- Cleanup: a periodic task removes expired upload dirs; the coordinator also
  removes the staged dir after the build completes (success or failure).

Build path — `src/artifacts/acquirer.rs`:
- Add `build_from_staged_context(runner, staged_dir, context_path,
  dockerfile_path)` = `build_from_git_checkout` minus clone/checkout: call
  `confine_under(staged_dir, context_path)` and `confine_under(staged_dir,
  dockerfile_path)`, then run the identical `buildctl … --output type=oci`
  invocation. `acquire` dispatches `UploadedContext` to it.

Coordinator — `src/deploy/coordinator.rs`:
- Map `DeploymentRequest::Upload { upload_id, dockerfile_path, context_path }`
  → `ArtifactSource::UploadedContext { staged_dir =
  data_dir/uploads/<upload_id>/context, dockerfile_path, context_path }`.
- After the run, delete `data_dir/uploads/<upload_id>` regardless of outcome.

### 4.6 `denia push` — `src/cli/client/push.rs` (new), `Commands::Push`

`PushArgs`: `--create`, `--project`, `--service`, `--dockerfile`, `--context`,
`--path` (default `.`), `--profile`, `--no-follow`.

Flow:
1. Load active profile (`client.toml`) and read `.denia` from `--path`. Flags
   override manifest fields.
2. Resolve service: `GET /v1/services`, match by project + service name → id.
   - Missing service: error unless `--create`.
   - `--create`: resolve/`POST /v1/projects` if the project is missing, then
     `POST /v1/services` with `[create]` defaults (port required, optional
     `health_path`); write any new names back into `.denia`.
3. Assert the Dockerfile exists at `context/dockerfile`; else fail
   `no Dockerfile found at <path> (required)`.
4. Pack the working tree (`pack.rs`) → `tar.zst` temp file.
5. `POST /v1/services/{id}/uploads` streaming the archive → `{upload_id}`
   (byte-progress indicator).
6. `POST /v1/deployments { source: "upload", service_id, upload_id, dockerfile,
   context }` → `202` + deployment id.
7. Unless `--no-follow`: tail the existing per-deployment SSE log stream until
   the deployment reaches `Healthy` or `Failed`; exit non-zero on `Failed`.
   Print the deployment id (and service URL when resolvable).

New `ClientApi` methods: `list_projects` (exists), `create_project`,
`create_service`, `upload_context` (streaming), `create_deployment`,
`stream_deployment_log`.

### 4.7 CLI wiring — `src/cli/mod.rs`

Add `Commands::Auth(client::auth::AuthArgs)` and
`Commands::Push(client::push::PushArgs)`; dispatch builds a tokio runtime for
each (like `Console`).

## 5. Data Flow (push)

```
working tree
  └─(pack: git ls-files ∩ .dockerignore, +Dockerfile)→ tar.zst (temp)
      └─POST /v1/services/{id}/uploads (stream, body-cap)→ uploads/<id>/context.tar.zst
          └─hardened extract→ uploads/<id>/context/
              └─POST /v1/deployments {source:upload, upload_id}→ 202 + deployment id
                  └─coordinator → UploadedContext → acquirer buildctl --output type=oci
                      └─rootfs bundle → LinuxRuntime health-gated promote (ADR-024)
                          └─SSE deploy log → CLI (until Healthy/Failed)
```

## 6. Security / Trust Boundaries

- **Untrusted tarball extraction is a new host-root trust surface.** Mitigations
  in §4.5: no absolute/`..`/escaping-symlink/special members; uncompressed-size
  + entry-count caps; per-request body cap. The build itself runs at the same
  privilege as today's git build (no new build privilege introduced).
- **Credentials:** `denia auth` persists only the long-lived API token (`0600`),
  never the password or session token. The token is named and revocable from the
  web console.
- **Authz:** both `POST /uploads` and `POST /deployments` require project
  **Operator**; Viewer is `403`. Upload paths derive from a server-generated
  UUIDv7, never from client-supplied names.

## 7. Testing

Unit:
- Manifest: new optional fields parse; 2-field back-compat; relative path
  resolution.
- Packer: `.gitignore` + `.dockerignore` honored; Dockerfile always included;
  non-git directory walk; size/count guards.
- Profile writer: round-trip, `0600`, atomic replace.
- Hardened untar: table tests rejecting absolute paths, `..`, escaping
  symlink/hardlink, special files, and oversized / too-many-entry archives.
- Auth + service resolution + `--create` against a mock HTTP server.

API:
- `POST /uploads`: body-size cap → `413`; Operator required, Viewer `403`;
  malformed archive → `400`.
- `POST /deployments` with `upload` source: happy path maps to
  `UploadedContext` and deletes the staged dir afterward.

Privileged (gated, `DENIA_RUN_PRIVILEGED_TESTS=1`):
- Full upload → build → deploy → `Healthy` on a tiny Dockerfile.

## 8. ADR Reconciliation

- **ADR-035 (new):** "Client-Driven Deploy via Working-Tree Upload" — records the
  upload endpoint, the `UploadedContext` artifact source, the Vercel-style
  working-tree context model, and the auth token-minting client flow.
- **ADR-030:** restore onto master with status **Superseded by ADR-035** (its
  Git-deploy mechanism is replaced; its cross-platform packaging decision is
  left to a future ADR and noted as out of scope here).
- Update `docs/adr/README.md` index with ADR-030, ADR-035 rows (ADR-032 already
  restored separately).
- Update root `README.md` (Features, CLI subcommands, API highlights, a "Deploy
  from your machine" section) once implemented.

## 9. File-Level Change Summary

New:
- `src/cli/client/auth.rs`, `src/cli/client/push.rs`, `src/cli/client/pack.rs`
- `src/api/uploads.rs` (or fold into `src/api/deployments.rs`)
- `docs/adr/035-client-driven-deploy-upload.md`

Modified:
- `src/cli/client/profile.rs` (writer), `src/cli/client/manifest.rs` (fields),
  `src/cli/client/http.rs` (new client methods), `src/cli/client/mod.rs`,
  `src/cli/mod.rs` (subcommands)
- `src/domain/deployment.rs` (`Upload` variant), `src/artifacts/mod.rs`
  (`UploadedContext` source), `src/artifacts/acquirer.rs`
  (`build_from_staged_context`), `src/deploy/coordinator.rs` (mapping + cleanup)
- `src/api/router.rs` / app router (mount `/uploads`)
- config (`src/config.rs`) for upload caps + TTL
- `docs/adr/README.md`, root `README.md`
```
