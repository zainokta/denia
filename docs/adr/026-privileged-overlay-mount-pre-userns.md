# ADR-026: Privileged Overlay Mount Before the User-Namespace Unshare

- **Status**: Accepted
- **Date**: 2026-05-29
- **Amends**: ADR-019 (Per-Replica Runtime Filesystem Isolation)

## Context

ADR-019 mounts the per-replica overlay rootfs "in the workload's mount namespace
before `pivot_root`". As implemented, that mount happened **inside the workload's
unprivileged user namespace**: `src/syscall/ns.rs` did a single
`unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | …)` and then called
`mount("overlay", …)` in the post-unshare child, where the child holds
`CAP_SYS_ADMIN` only against its own user namespace, not the initial one.

On a **btrfs** data directory (`/var/lib/denia`) this mount fails:

```
ChildSetup { stage: "chroot: overlay mount errno=13 (EACCES)" }
```

with the kernel logging `overlayfs: upper fs does not support tmpfile`. Mounting
overlayfs-as-upper inside an unprivileged user namespace is fragile on btrfs (the
kernel's `tmpfile`/xattr probes against the btrfs upper fail in that restricted
context). Two attempted fixes did not help and were reverted in spirit by this
ADR's change:

- appending `,userxattr` to the mount options (use `user.overlay.*` xattrs), and
- recursively `chown`-ing the layers to `userns_base` (so they are owned by a
  uid mapped into the namespace).

Both addressed a permission model that was already satisfied; the blocker is the
unprivileged-userns + overlay-on-btrfs combination itself. This was confirmed on
the affected host: a plain privileged `mount -t overlay …` (no `userxattr`) on the
same btrfs directories succeeds, while the in-userns mount fails.

## Decision

Mount the overlay **privileged, in the initial user namespace**, before the
workload enters its own user namespace. The child process retains the daemon's
initial-namespace `CAP_SYS_ADMIN` across `fork()` until it unshares
`CLONE_NEWUSER`, so the post-fork (single-threaded) child performs a **two-stage
unshare**:

1. `unshare(CLONE_NEWNS)` — create a private mount namespace while still in the
   initial user namespace. Set `/` to `MS_REC | MS_PRIVATE` (so the mount does
   not propagate into the host's `shared` mount table), then `mount` the overlay
   (or, for the non-overlay path, self-bind the rootfs). This is a normal
   privileged mount: `trusted.overlay.*` xattrs and `tmpfile` work on btrfs, so
   `userxattr` is no longer used.
2. `unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | …)` — the full set. The
   re-applied `CLONE_NEWNS` makes the new mount namespace (a copy carrying the
   already-mounted overlay) **owned by the new user namespace**, so the child can
   `pivot_root` and mount `/proc` with `CAP_SYS_ADMIN` over its own user ns.

The parent's id-map handshake is unchanged: it still writes `uid_map`/`gid_map`
after the child signals post-unshare readiness, which now occurs after stage 2.

The per-replica `chown` of the writable `upper` layer (and guest socket dirs) to
`userns_base` is **kept**, but now only so the workload owns its writable layer
and its runtime writes/copy-ups land with the guest uid — not as a precondition
for the mount to succeed.

Read-only **bind mounts move into the same pre-userns stage**. They were
previously applied in `child_exec`, after `pivot_root`, inside the workload's
unprivileged user namespace — binding `/.old_root/<host path>` and then
remounting read-only. That fails the same way:

```
ChildSetup { stage: "read-only bind mount errno=13 (EACCES)" }
```

The bind **source** is a host-absolute path (the daemon binary, used as the
socket-proxy via `current_exe()`). When that path traverses a directory owned by
a uid **not mapped** into the workload namespace — e.g. a developer's `0700`
home for a `cargo run`, or any non-world-searchable ancestor — the child's
`CAP_DAC_*` does not apply (capabilities override DAC only for mapped owners), so
it falls back to the `other` permission bits and the first `MS_BIND` fails
`EACCES` on source-path resolution. (`errno=13`, not the `EPERM=1` a remount
privilege failure would give.) The remount-to-read-only additionally needs
`CAP_SYS_ADMIN` over the host superblock, which the in-userns child lacks.

So `child_prepare_root` now also applies the read-only binds, **onto the new-root
base** (`merged` for the overlay path, the artifact `rootfs` otherwise), using the
raw host source path with no `/.old_root` prefix. Run privileged in the initial
user namespace, both the `MS_BIND` and the `MS_REMOUNT|MS_RDONLY` succeed
regardless of the source path's permissions. The recursive self-bind
(`MS_BIND|MS_REC`) and `pivot_root` in `child_exec` carry the binds into the
workload, where they are `MNT_LOCKED` read-only — the workload cannot remount
them writable. Production (binary under world-searchable `/usr/local/bin`) was
unaffected by the old code; this makes the runtime source-path-independent.

The guest `/dev` is built in the same pre-userns stage. A fresh `tmpfs` is
mounted over `<base>/dev` and the host's character device nodes (`null`, `zero`,
`full`, `random`, `urandom`, `tty`) are **bind-mounted** onto it from the
initial namespace. `mknod` is rejected `EPERM` inside the unprivileged workload
userns, and the systemd unit's `AmbientCapabilities` omit `CAP_MKNOD`, so a bind
(needing only `CAP_SYS_ADMIN`) is the only portable way to give the workload
working device nodes. The previous in-userns `mknod`-then-`symlink` fallback
produced a circular `/dev/null` symlink that broke the socket-proxy's
`Stdio::null()` and any workload opening `/dev/null`. The recursive self-bind +
`pivot_root` carry the `/dev` tmpfs and its node binds into the workload.

**socket-proxy runtime libraries.** socket-proxy is the daemon binary itself
(`current_exe()`), dynamically linked against the host glibc + loader. Bound as a
lone file into an arbitrary workload image, its loader resolves `libc.so.6`,
`libm.so.6`, `libgcc_s.so.1` against the *image's* libs and fails (missing lib, or
a glibc-version mismatch) before it can bind the guest socket — surfacing to the
control plane as `ServiceSocketUnavailable`. To make socket-proxy
image-independent, its host shared objects + dynamic loader (resolved from the
daemon's own `/proc/self/maps`) are bound read-only under `/.denia/lib`, and
socket-proxy is launched through the bound loader:
`/.denia/lib/<loader> --library-path /.denia/lib /.denia/socket-proxy …`. The
`--library-path` is consumed by the loader at socket-proxy startup and is **not**
inherited by the workload socket-proxy spawns, so the workload keeps using its own
image libc. A statically-linked socket-proxy yields no libs and is exec'd directly.

**Workload socket on host fs (not the overlay).** socket-proxy's listening unix
socket must be reachable by the daemon's Pingora ingress. A unix socket created
on overlayfs (the workload's `merged` view) is bound to the **overlay inode** and
is **not** connectable via the underlying upperdir path — the host-side daemon
gets `ECONNREFUSED`. So a per-replica directory on the **real host fs**
(`<data_dir>/sock/<hash>/`) is **read-write bind-mounted onto the same absolute
path inside the guest** (identity mount via `with_socket_bind`, applied pre-userns
alongside the overlay/`/dev` binds and chowned to `userns_base`). socket-proxy
binds `service.sock` there, so it is the **same inode** for both guest and host.

The mount is **identity** (guest path == host path) deliberately. Pingora
validates a pooled UDS connection by comparing `getpeername(fd)` — the server's
bound `sun_path` — against its dial path (`ConnFdReusable for Path`,
`pingora-core/src/protocols/mod.rs`). If socket-proxy binds a *guest-only* path
(the earlier `/run/denia/service.sock`) while the daemon dials the host path, the
two `sun_path` strings never match even though it is the same inode, so the check
fails on **every** reuse — logging `Crit: unix FD mismatch: peer: /run/denia/…,
addr: /var/lib/denia/sock/<hash>/…` and discarding the pooled connection to open a
fresh one per request (requests still succeed; UDS keep-alive pooling is silently
disabled). Binding and dialing the identical string keeps the reuse check passing.

That path is also short — under the `sockaddr_un` 108-byte limit (the hashed dir
keeps it ~49 bytes) — whereas the deep per-replica overlay path
(`…/upper/run/denia/service.sock`, ~127 bytes) both exceeds the limit and is the
unconnectable overlay inode. The guest path the workload sees in the
`DENIA_SERVICE_SOCKET` env is this same host path. The daemon/ingress connect via
the host path.

The two-stage unshare now fires whenever an overlay is configured, any read-only
bind is requested, `/dev` setup is requested, **or** a socket bind is configured.

**Socket bind mountpoint safety amendment (2026-06-01).** The pre-userns socket
bind still runs privileged for the reasons above, but its destination path is
image-influenced because the overlay `merged` tree contains rootfs symlinks. When
creating the mountpoint chain for the identity socket bind, the child must not
follow existing symlink components. Each existing component is checked with
`lstat`; only real directories are accepted. Symlinks fail the child setup before
the bind mount is attempted.

**Workdir normalization.** OCI `WorkingDir` is attacker-influenced through the
image config, so the runtime validates it before planning or forking. Workdirs
must be absolute, must not contain NUL, `.` or `..` components, and are created
under the selected new root by stripping the leading `/` before joining. Setup
errors are reported through the child setup pipe instead of being ignored.

## Consequences

- Workload deploys succeed on btrfs (and on any upper filesystem), eliminating the
  `EACCES`/`tmpfile` failure class for unprivileged-userns overlay mounts.
- The runtime no longer depends on kernel support for overlayfs *inside* an
  unprivileged user namespace; it only requires the daemon to hold `CAP_SYS_ADMIN`
  in the initial namespace (it already does, via root or the systemd unit's
  `AmbientCapabilities`).
- `userxattr` is dropped from the overlay options.
- A new child-setup failure tag (`'X'`) distinguishes a failure in the second
  (user/pid) unshare from the first (mount-ns) unshare (`'U'`).
- Read-only bind mounts are source-path-independent: a daemon binary under a
  `0700` home (dev `cargo run`) deploys identically to a world-searchable
  `/usr/local/bin` install. The `/.old_root`-relative bind source is gone.
- Malicious image workdirs can no longer create host directories outside the
  workload root before `pivot_root`.
- Malicious rootfs symlinks on the socket bind destination prefix can no longer
  make privileged pre-userns mountpoint creation walk outside the selected new
  root.

## Risks

- **Mount propagation**: the stage-1 `MS_REC | MS_PRIVATE` on `/` must precede the
  overlay mount, or the mount leaks into the host's `shared` table. `MountFlags=shared`
  in the unit is retained and is exactly why the child sets its tree private.
- **`CLONE_NEWPID` placement**: kept in the second unshare so the existing
  post-NEWPID fork still makes the workload pid 1.
- **`unshare(CLONE_NEWUSER)` single-thread requirement**: still satisfied because
  the unshares run in the post-`fork()` child, which is single-threaded.

## Alternatives Considered

- **Mount in the daemon before `fork()`**: pollutes the daemon/host mount table
  (it is `shared`) and needs explicit cross-namespace cleanup. Rejected in favour
  of mounting in the child's own private mount namespace.
- **tmpfs- or ext4-loopback-backed upper/work**: side-steps the btrfs quirk but
  makes the writable layer RAM-backed (needs a size cap) or adds loop-device
  lifecycle management. Rejected as more invasive than relocating the mount.
- **Keep the in-userns mount, fix via `userxattr` + ownership**: already tried;
  does not address the root cause. Rejected.

## References

- ADR-019 (Per-Replica Runtime Filesystem Isolation) — amended by this ADR
- ADR-003 (Linux Runtime Process Runner)
- `src/syscall/ns.rs` — `child_stage1`, `child_prepare_root`, `child_apply_ro_bind`, `child_setup_dev`, `child_bind_dir_rw`, `child_exec`
- `src/runtime/linux.rs` — per-replica layer preparation, ownership, and the socket-proxy read-only bind
