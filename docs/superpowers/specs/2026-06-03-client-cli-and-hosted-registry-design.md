# Cross-Platform Client CLI And Hosted Registry Design

- **Date**: 2026-06-03
- **Status**: Approved (brainstorm)
- **Scope**: Backend, CLI packaging, release packaging, and web console registry UI. Runtime isolation behavior is unchanged.

## Problem

Denia needs two connected operator workflows:

1. A local project deploy flow: authenticate once, keep a `.denia` project
   manifest, and run `denia push` from the current branch.
2. Container registry support.

Denia already has a Linux host binary named `denia`, project-scoped external
registry configuration, SOPS-encrypted registry credentials, and a service-centric
deploy API. That solves private pulls from external registries, but not a
cross-platform client CLI and not a Denia-hosted image registry.

The current Linux binary also contains host-only assumptions: systemd setup,
glibc release checks, Pingora ingress, cgroup/runtime syscalls, and privileged
Linux runtime code. A developer on macOS or Windows should not need the host
daemon/runtime stack to run `denia auth` or `denia push`.

## Goals

- Keep the user-facing client command as `denia`.
- Support cross-platform client builds for Linux, macOS, and Windows.
- Keep Linux host/server actions available, but group them under
  `denia server ...`.
- Add `denia auth` to create and store a Denia API token.
- Add `denia push` to deploy the pushed remote Git branch through existing
  `/v1/services` and `/v1/deployments`.
- Keep `.denia` as a committed, non-secret TOML deploy manifest.
- Design Denia-hosted OCI registry support under same-origin `/v2`.
- Store hosted registry blobs locally under `data_dir`.
- Include hosted registry cleanup and web UI in the design.

## Non-Goals

- Deploying uncommitted local working tree snapshots.
- Building an OCI image on the client in the first CLI phase.
- Replacing external registry configuration from ADR-014/ADR-021.
- Multi-node registry replication.
- Object storage-backed registry blobs in the first hosted-registry phase.
- Docker/Podman login as the first auth path. The registry initially reuses
  Denia bearer API tokens; Docker-compatible login can be added after the
  Denia-controlled path works.

## Decisions

| Topic | Decision |
|---|---|
| Client command | Installed command remains `denia`. |
| Server command boundary | Linux server-capable builds expose host actions under `denia server ...`. |
| Client distribution | GitHub Release binaries for Linux/macOS/Windows. |
| Auth | `denia auth` logs in with username/password, creates a named API token, stores URL + token in a local profile. |
| Token storage | Store in a user config file with `0600` permissions where supported. |
| `.denia` | Committed TOML deploy manifest, no secrets. |
| Push source | Current branch remote Git commit, not local snapshot. |
| First source type | Git + Dockerfile only. |
| Git credential | `.denia` references an existing Denia Git credential secret ref. |
| Service behavior | `denia push` creates or updates the service, then deploys. |
| Hosted registry endpoint | Same-origin `/v2`. |
| Hosted registry auth | Denia API token bearer auth first. |
| Hosted image names | `<project>/<service>`. |
| Hosted registry storage | Local `data_dir` with periodic/manual cleanup and UI. |

## Client CLI Design

The crate should split host/server code from client code. The installed command
is still `denia`, but the command tree becomes:

```text
denia auth
denia push
denia profile list
denia profile use <name>
denia server run
denia server setup
denia server status
denia server doctor
denia server rotate-token
denia server update
denia server uninstall
```

Cross-platform client builds include the client command set. Linux server-capable
builds include both client commands and `server` commands. Running `denia` with
no subcommand on a server-capable build should continue to run the daemon for
compatibility during migration, but `denia server run` becomes the explicit
server entrypoint and the systemd unit should use it.

Client profiles live under the user's config directory:

```toml
active = "default"

[[profiles]]
name = "default"
url = "https://denia.example.com"
token = "64-byte-hex-or-future-token-value"
```

The file must be created with owner-only permissions on Unix. On platforms that
do not expose Unix mode bits, the CLI should still write the profile but warn
if it can detect broad access.

`denia auth` flow:

1. Prompt for Denia URL.
2. Prompt for username and password.
3. POST `/v1/auth/login`.
4. POST `/v1/api-tokens` with a name like `denia-cli-<hostname>`.
5. Store the returned API token in the selected profile.
6. Verify the stored token with `/v1/me`.

`.denia` manifest:

```toml
version = 1
project = "default"
service = "api"

[source]
type = "git"
remote = "origin"
dockerfile = "Dockerfile"
context = "."
git_credential_ref = "deploy-key"

[runtime]
internal_port = 8080

[health]
path = "/"
timeout_seconds = 5

[limits]
cpu_millis = 500
memory_bytes = 536870912

[ingress]
domains = ["api.example.com"]
tls_enabled = true
```

`denia push` flow:

1. Load the active profile and `.denia`.
2. Resolve the Git remote URL and current branch.
3. Verify `HEAD` equals the upstream remote branch. If not, fail with a message
   telling the user to push Git first.
4. GET `/v1/projects`, find the named project visible to the user.
5. GET `/v1/services`, find service by project id and service name.
6. POST `/v1/services` with a full `ServiceConfig`, reusing service id when it
   already exists.
7. POST `/v1/deployments` with a Git deployment request.
8. Print deployment id and a web URL for the deployment detail/log view.

## Hosted Registry Design

Hosted registry is a separate backend subsystem from external pull registries.
External registries remain project-scoped credential records. Hosted registry is
a Denia-owned content store and Distribution-style API.

The hosted registry API is mounted at `/v2` on the same origin as the management
API. It must not be nested under `/v1`.

Repository names map to project and service names:

```text
/v2/<project>/<service>/blobs/uploads/
/v2/<project>/<service>/blobs/<digest>
/v2/<project>/<service>/manifests/<tag-or-digest>
```

Auth initially reuses Denia bearer API tokens. Push requires project Operator or
Admin. Pull requires project Viewer unless a future public-pull policy is added.
The first implementation should be optimized for Denia CLI-controlled pushes,
but the route and storage shape should be compatible with standard OCI clients.

Storage lives under `data_dir/registry`:

```text
registry/
  blobs/sha256/<digest-hex>
  uploads/<uuidv7>/
    data
    state.json
```

SQLite stores metadata:

- repositories: project id, service id, name, created_at
- manifests: repository id, digest, media type, size, blob path/ref, created_at
- tags: repository id, tag, manifest digest, updated_at
- blob links: repository id, digest, size, created_at
- upload sessions: id, repository id, path, started_at, updated_at
- GC runs: status, scanned/deleted counts, reclaimed bytes, timestamps

Garbage collection has two entrypoints:

- periodic registry GC task controlled by config;
- manual `POST /v1/registry/gc` for super-admins.

GC keeps blobs referenced by manifests, referenced by active uploads, or younger
than the configured grace period. Everything else can be removed from disk and
metadata in one run report.

The web console should extend the existing registry/cache UI patterns:

- existing `/registries` remains external pull registry credential management;
- add hosted registry visibility showing repositories, tags, digest, size, last
  push, and service/project linkage;
- add node-wide hosted registry storage/GC status near the existing OCI layer
  cache settings area.

## Security

- Never log API tokens, passwords, registry credentials, or uploaded layer
  payloads.
- `/v2` must reuse the existing bearer auth resolution path but must have its own
  project role checks.
- Repository path segments must resolve to existing project/service names and
  reject traversal, empty names, uppercase ambiguity if names are normalized, and
  names that do not match Denia's service/project naming rules.
- Upload session IDs must be UUIDv7.
- Blob digest verification is mandatory before committing an uploaded blob.
- GC must never remove blobs referenced by any manifest or in-progress upload.

## Testing

- Client unit tests: profile parsing, profile permissions, manifest parsing,
  Git state checks, URL normalization.
- Client integration tests: mocked login/token creation, service upsert,
  deployment creation, auth failure, unpushed branch failure.
- Backend tests: `/v2` auth, push upload lifecycle, digest verification,
  manifest/tag persistence, project/service namespace checks, GC keep/delete
  behavior.
- Web tests: hosted registry list/detail/GC status states.

## References

- ADR-001: Initial Backend Architecture
- ADR-014: Per-Service OCI Registry Configuration
- ADR-021: Control-Plane SOPS Secret Encryption
- ADR-025: CLI-Driven Host Provisioning
- ADR-029: Self-Update From Signed GitHub Release Binaries
