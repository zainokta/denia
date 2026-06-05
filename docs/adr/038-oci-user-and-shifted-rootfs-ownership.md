# ADR-038: OCI User and Shifted Rootfs Ownership

- **Status**: Accepted
- **Date**: 2026-06-05
- **Amends**: ADR-003 (Linux Runtime Process Runner), ADR-026 (Privileged Overlay Mount Before the User-Namespace Unshare)

## Context

Denia maps container uid/gid `0` to the configured host `DENIA_USERNS_BASE`.
Before this ADR, rootfs materialization recursively changed every unpacked entry
to that host uid/gid and runtime launch ignored OCI `config.User`. That made
root images work, but broke images that intentionally run as a non-root user and
pre-create writable paths for that user.

The `denia-documentation` image is one such case. It uses
`nginxinc/nginx-unprivileged`, declares `USER 101`, and prepares
`/tmp/client_temp` as `101:101`. Denia flattened that path to container root,
then started nginx as root inside the user namespace. Nginx attempted to chown
the temp directory to uid 101 and failed after helper hardening removed the
needed capabilities:

```
chown("/tmp/client_temp", 101) failed (1: Operation not permitted)
```

The socket proxy then reported the downstream symptom:

```
socket-proxy: upstream connect 127.0.0.1:8080 failed: Connection refused
```

## Decision

Denia preserves OCI ownership and launch user semantics:

- Rootfs unpack restores raw numeric tar uid/gid when the daemon has `CAP_CHOWN`.
  Directory ownership is applied after layer extraction, in reverse order, so
  chowning a directory away from the daemon does not block later writes into it.
- Rootfs materialization shifts image ids into Denia's configured user namespace
  range. Image uid/gid `N` becomes host uid/gid `DENIA_USERNS_BASE + N`. Entries
  already inside the configured shifted range are left unchanged.
- Each rootfs bundle records a `rootfs.ownership.json` sidecar containing the
  ownership schema version and user namespace range. A bundle without a matching
  sidecar is republished from its OCI layers instead of reused.
- `process.json` includes a defaulted `user` object with `uid` and `gid`.
  Missing `user` means `0:0`, preserving compatibility with older bundles.
- OCI `config.User` is resolved during artifact materialization. Numeric ids,
  user names from `/etc/passwd`, group names from `/etc/group`, and
  `user:group` forms are supported. Unknown names or ids outside
  `DENIA_USERNS_SIZE` reject acquisition with a clear error.
- `socket-proxy` and `workload-launcher` keep running as userns root for their
  Denia setup/hardening work, then spawn the actual workload child with the
  manifest uid/gid.

## Consequences

- Non-root OCI images can rely on image-owned writable directories and their
  declared `USER`.
- Existing root/default images continue to launch as container uid/gid `0:0`.
- Existing bundles missing ownership metadata are repaired by reacquiring the
  rootfs from layers on the next deployment/acquisition.
- The runtime still depends on the configured user namespace range being large
  enough for image uid/gid values that operators deploy.

## Alternatives Considered

- **Patch individual images to run as root**: rejected because it leaves Denia
  incompatible with ordinary non-root OCI images.
- **Only honor `USER` but keep flattened ownership**: rejected because non-root
  images also rely on ownership prepared in image layers.
- **Run socket-proxy itself as the image user**: rejected because the helper must
  bind Denia's service socket, bring loopback up, and apply hardening before
  spawning the workload.

## References

- ADR-003: Linux Runtime Process Runner
- ADR-026: Privileged Overlay Mount Before the User-Namespace Unshare
- `src/artifacts/acquirer.rs`
- `src/oci/unpack.rs`
- `src/ingress/socket_proxy.rs`
- `src/workload_launcher.rs`
