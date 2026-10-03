# ADR-031: Hosted OCI Registry

- Status: Accepted
- Date: 2026-06-03

## Context

Denia already supports project-scoped external registry credentials for pulling
private images (ADR-014 and ADR-021). That is not the same as Denia hosting an
OCI registry. A hosted registry lets Denia store images under its own
`data_dir`, support future client-side builds, and eventually accept pushes from
standard OCI clients.

ADR-001 intentionally deferred hosted registry support because a push-compatible
registry needs upload sessions, blob integrity checks, auth scopes, tag/manifest
metadata, retention, and garbage collection. The feature is now explicitly in
scope, but Denia remains a single-node PaaS; multi-node replication and object
storage are not accepted here.

## Decision

Add a Denia-hosted OCI registry as a separate subsystem from external pull
registries.

Expose the registry on the same origin at `/v2`, outside `/v1`. The management
API remains under `/v1`; `/v2` follows the OCI Distribution route shape:

```text
/v2/
/v2/<project>/<service>/blobs/uploads/
/v2/<project>/<service>/blobs/uploads/<upload_id>
/v2/<project>/<service>/blobs/<digest>
/v2/<project>/<service>/manifests/<tag-or-digest>
```

Repository names map to Denia project/service names. Push requires project
Operator or Admin. Pull requires project Viewer. The first implementation
authenticates with the same bearer API tokens used by `/v1`; Docker-compatible
login/token exchange can be added after the Denia CLI-controlled path is stable.

Store registry content under `data_dir/registry`:

```text
registry/
  blobs/sha256/<digest-hex>
  uploads/<uuidv7>/
    data
    state.json
```

Persist metadata in SQLite for repositories, manifests, tags, blob links,
upload sessions, and GC runs. Uploaded blob data is written to an upload session
file, verified against the requested digest, then atomically moved into the
content-addressed blob path.

Add registry garbage collection:

- periodic task controlled by registry GC config;
- manual super-admin endpoint under `/v1/registry/gc`;
- status endpoint for UI visibility.

GC preserves blobs referenced by manifests, blobs linked to active uploads, and
blobs younger than the configured grace period. Unreferenced eligible blobs are
removed from disk and metadata with a run report.

Add web console visibility:

- keep `/registries` for external pull registries;
- add hosted registry repository/tag visibility;
- add hosted registry storage and GC status near the existing OCI layer cache
  settings surface.

## Consequences

- Easier: Denia can own image storage for future local-build push workflows.
- Easier: registry storage, GC, and service linkage are visible in the same
  operator console as deployments and cache state.
- Easier: same-origin `/v2` avoids a required dedicated registry hostname for
  the first version.
- Harder: `/v2` must implement enough Distribution behavior for reliable push
  and pull semantics.
- Harder: registry GC must be conservative; deleting a referenced blob breaks
  deployments and clients.
- Harder: local blob storage grows the `data_dir` footprint and needs operator
  observability.
- Constraint: storage is local single-node only. S3-compatible storage,
  multi-node replication, public repositories, and Docker-compatible login are
  future ADRs or amendments.

## Alternatives Considered

- **External registries only.** Rejected because it does not satisfy Denia-owned
  registry support or future local-build push workflows.
- **Dedicated `registry.example.com` host first.** Rejected for the first
  version because same-origin `/v2` is simpler for single-node installs and
  avoids additional domain/TLS policy.
- **S3-compatible blob storage first.** Rejected because it adds credentials,
  lifecycle policy, dependency, and failure-mode surface before the single-node
  storage model is proven.
- **SQLite blob storage.** Rejected because large layer blobs are a poor fit for
  SQLite; SQLite should own metadata, not layer payloads.
- **Docker Basic auth first.** Rejected because Denia already has bearer API
  tokens and project RBAC; Docker-compatible login can map to scoped tokens
  later.

## Amendment (2026-06-03): Docker Basic auth for push/pull

`/v2` additionally accepts HTTP Basic auth where the password is a Denia API
token (`docker login <host> -u <user> -p <api-token>`); the username is ignored.
Unauthenticated `/v2` responses now advertise `WWW-Authenticate: Basic
realm="Denia Registry"` so standard docker clients perform the login handshake.
This is the first increment of the "Docker-compatible login" future work noted
above; OAuth2 token-endpoint exchange remains out of scope. Adds the `base64`
dependency for decoding Basic credentials.

## Amendment (2026-10-02): Denia-managed Zot

Zot replaces the hosted registry data plane and its garbage collection. Denia
keeps authentication, project RBAC, and repository ownership metadata. Zot runs
as `denia:denia`, binds only to `127.0.0.1:5000`, and stores content under
`/var/lib/denia/zot`. Denia remains the public `/v2` endpoint. The existing
private external-registry credential path and rootfs unpacker are unchanged.

The release pins Zot 2.1.20 and architecture-specific SHA-256 digests. Setup and
update share one idempotent reconciliation operation: verify the binary,
validate the loopback config, install the managed files, reload systemd as
needed, enable/start Zot, and wait for it to become active. Changed running Zot
instances must restart to load their new binary/config. Exact matching managed
binaries are reused without downloading.

After Zot is active, reconciliation installs a Denia unit drop-in declaring
`Requires=zot.service` and `After=zot.service`. This preserves dependency ordering
on boot for existing installations without rewriting their Denia unit.

An ownership record binds the installed binary to its verified hash. A
conflicting unrelated binary or service is an error; Denia must not take it over.
Failed reconciliation restores prior managed files and attempts service
recovery. A later Denia restart failure does not roll back successfully
reconciled Zot or delete registry data. Automatic rollback covers executable
and configuration files, not Zot storage format changes; future pins must be
reviewed for storage compatibility.

Uninstall checks the same ownership evidence before stopping or removing Zot.
Ordinary uninstall retains the binary, configuration, and ownership record for
setup to reuse. Purge removes those owned files as well as Denia data. Both modes
remove the managed Denia dependency drop-in; unrelated Zot files and services
are preserved.

The management GC endpoint reports that Zot performs GC automatically, rather
than starting Denia's old hosted-registry sweep. Hosted-registry status/tag
information comes from Zot. Existing legacy registry content is not imported
by this dependency provisioning change.

## References

- ADR-001 (initial backend architecture)
- ADR-014 (per-service OCI registry configuration)
- ADR-021 (control-plane SOPS secret encryption)
- ADR-030 (cross-platform client CLI)
