# ADR-030: Cross-Platform Client CLI

- Status: Superseded by ADR-034 (deploy mechanism); cross-platform delivery resolved by ADR-037
- Date: 2026-06-03

> **Superseded.** The Git-based `denia push` mechanism described here is replaced
> by ADR-034 (Client-Driven Deploy via Working-Tree Upload). The `denia auth`
> token-minting flow below is retained as-is by ADR-034. The cross-platform
> client/server command split (`denia server …`, client-only build profiles) is
> **not** adopted by ADR-034; cross-platform delivery is resolved differently in
> ADR-037 (a cfg-gated single `denia` crate), which keeps `auth`/`push`/`console`
> top-level on every platform rather than introducing a `denia server` group.

## Context

Denia currently ships a Linux host binary named `denia`. That binary owns
daemon startup, host provisioning, systemd operations, runtime isolation,
Pingora ingress, and self-update. Operators need a local project deploy
workflow: authenticate once, keep a `.denia` project file, and run `denia push`
from a local branch.

That client workflow must run on developer machines, not only on the hosted
Denia node. macOS and Windows users should not compile or install Linux runtime
isolation code, systemd helpers, cgroup logic, or glibc-gated host release
checks just to push an app.

## Decision

Keep the installed client command as `denia`, but split the command surface into
client actions and server actions:

- client actions stay top-level: `denia auth`, `denia push`, and profile
  commands;
- host/server actions move under `denia server ...`: `run`, `setup`, `status`,
  `doctor`, `rotate-token`, `update`, and `uninstall`.

Cross-platform client builds include only the client command set and shared
client-side types. Linux server-capable builds include both client actions and
the `server` command group. The systemd unit should call `denia server run`.
During migration, a server-capable Linux build may keep "no subcommand starts
the daemon" compatibility, but explicit server mode is the documented entrypoint.

Add a committed `.denia` TOML manifest for project deploy settings. It contains
no secrets. The manifest describes project name, service name, Git source,
Dockerfile/context paths, an existing Git credential reference, runtime port,
health check, limits, domains, and TLS intent.

Add `denia auth`:

1. prompt for Denia URL, username, and password;
2. call `/v1/auth/login`;
3. create a named API token via `/v1/api-tokens`;
4. store profile URL + token in a user config file with owner-only permissions
   where the platform supports them;
5. verify with `/v1/me`.

Add `denia push`:

1. read `.denia` and the active profile;
2. resolve the configured Git remote and current branch;
3. require local `HEAD` to match the upstream remote branch;
4. resolve the named project through `/v1/projects`;
5. create or update the service through `/v1/services`;
6. start a Git deployment through `/v1/deployments`;
7. print the deployment id and web URL.

Distribute client binaries through GitHub Releases for Linux, macOS, and
Windows. The existing Linux host release path from ADR-029 remains the server
artifact path.

## Consequences

- Easier: developers can install `denia` on any supported OS and deploy to a
  remote Denia node.
- Easier: `denia push` uses the existing service/deployment API and keeps build
  behavior on the Denia node.
- Easier: the host daemon command tree becomes explicit: `denia server ...`.
- Harder: packaging now has at least two build profiles: client-only and
  server-capable.
- Harder: shared domain/request types must be kept free of Linux-only
  dependencies if they are used by the client build.
- Constraint: the first `push` supports Git + Dockerfile only. Local snapshot
  upload and client-side image build are separate future decisions.

## Alternatives Considered

- **Keep all subcommands in the Linux host binary.** Rejected because it does not
  satisfy the cross-platform client requirement and forces client users through
  host-only dependencies.
- **Rename the client to `deniactl`.** Rejected because the desired user
  experience is `denia auth` and `denia push`.
- **Use one mixed top-level command tree.** Rejected because server-only actions
  such as setup/update would appear on client-only builds or create confusing
  platform-specific failures.
- **Deploy local working tree snapshots.** Rejected for the first version
  because Denia already has a Git/BuildKit deploy path and no source upload API.
- **Build and push images locally.** Rejected for the first client phase because
  hosted registry support is a separate subsystem.

## References

- [Spec: client CLI and hosted registry](../superpowers/specs/2026-06-03-client-cli-and-hosted-registry-design.md)
- ADR-025 (CLI-driven host provisioning)
- ADR-029 (signed GitHub release binaries)
