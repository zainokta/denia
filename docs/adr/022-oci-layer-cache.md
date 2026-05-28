# ADR-022: Persistent OCI Layer Cache With Weekly GC

- **Status**: Accepted
- **Date**: 2026-05-28

## Context

ADR-015 made layer staging streaming-friendly by routing each pulled compressed
layer through a per-pull `tempfile::TempDir` under `artifact_dir`. The temp dir
is reclaimed by RAII once `PulledImage` drops, which keeps the disk footprint
bounded but discards every cached byte after each deploy. Every subsequent
deploy — even a redeploy of the same image — re-streams every layer over the
network. For long-running PaaS workloads with frequent redeploys of the same
base images (a Node.js app rebuilt many times a day, a stable upstream image
shared by many services) this is wasted bandwidth and wasted deploy latency.

We also have a related operational hazard: an OCI layer cache that *only*
grows would silently consume the host's disk. Skopeo solved this externally;
we need an in-process equivalent that is bounded by a retention policy.

## Decision

1. **Content-addressed cache** rooted at `DENIA_OCI_CACHE_DIR` (default
   `<DENIA_DATA_DIR>/oci-cache/`). Each layer is stored at
   `oci-cache/blobs/<algorithm>/<digest_hex>` (mirroring the OCI distribution
   layout). Writes are atomic: download to `…/<digest>.tmp`, fsync, rename.
   Files are mode `0600`, owned by the Denia process user.

2. **Pull path checks cache first** by descriptor digest. On hit, verify per
   `DENIA_OCI_CACHE_VERIFY_ON_HIT` (`none` | `size` | `full`, default `size`)
   and reuse. On miss, stream from the registry into the cache, atomic-rename
   into place, then reuse.

3. **Atime tracking via sidecar `.lastref` files**. Each blob's last-reference
   time is the mtime of a sidecar `…/<digest>.lastref`. Touched on every put
   and every cache hit. We picked this over kernel atime because many production
   filesystems are mounted `noatime` or `relatime`, and the latter only updates
   atime when it is older than mtime; we'd silently retain blobs forever. We
   picked this over an in-memory `HashMap<Digest, SystemTime>` flushed to disk
   because the sidecar survives process restarts without an extra flush
   schedule and is trivially inspectable.

4. **Background garbage collector** runs every `DENIA_OCI_GC_INTERVAL_SECS`
   (default 7 days). A blob is collected only if **both** apply:

   - Its `.lastref` mtime is older than `DENIA_OCI_GC_RETENTION_SECS` (default
     7 days), **and**
   - It is not referenced by any currently-deployed service (queried from the
     promoted deployments and their artifact records) **and** not held by an
     in-flight pull reservation.

   "Currently pulling" is tracked by a per-cache `BTreeMap<Digest, usize>` of
   reservations protected by a `Mutex`. A pull holds a `CacheReservation` RAII
   guard that ref-counts the digest; GC refuses to delete a digest whose
   reservation count is non-zero. GC takes a coarse `RwLock` write while it
   walks; pulls take a read while they reserve. Pulls hold the *reservation*,
   not the lock, for the duration of the download.

5. **Observability**: `GET /v1/oci/cache` returns `{ entries, total_bytes,
   oldest_entry_age_secs, last_gc_at, last_gc_deleted_bytes }`. `POST
   /v1/oci/cache/gc` runs a synchronous sweep and returns the same payload
   plus `deleted_entries`. Both are super-admin gated.

6. **Defensive rails**:
   - GC refuses to operate if its root is not under `DENIA_DATA_DIR` or the
     configured `DENIA_OCI_CACHE_DIR` (defends against config corruption).
   - GC only deletes blobs it opens and stats in the same pass.
   - Logs never emit registry payloads — only digest + size + age.

## Consequences

- Layer re-pulls disappear for stable upstream images; deploy latency for
  redeploys drops to the artifact rebuild path.
- A bounded amount of additional disk is consumed under
  `oci-cache/blobs/sha256/`. The default 7-day retention scales with how
  often an image is redeployed; operators can tune the env vars.
- One new background task (`run_until_shutdown` pattern).
- `PulledImage._staging: Option<TempDir>` is now optional and unused on the
  registry pull path — cached blobs live in the persistent cache, not a
  temp dir. ADR-015's RAII reclaim is replaced by the reservation-based
  ref-count + scheduled GC described above.

## Alternatives Considered

- **Kernel atime**: rejected — most production filesystems disable or coalesce
  atime updates; we cannot rely on it.
- **LRU eviction at fixed cache size**: more complex (requires accurate
  per-blob size accounting under contention) and operationally surprising
  for an on-disk PaaS. A time-based retention with manual `POST .../gc` is
  predictable. Can be added later.
- **Reuse the existing artifact_dir for cache layout**: rejected — artifacts
  are per-image bundles; the cache is per-layer and shared across images.
  Mixing them confuses GC scope.

## References

- ADR-011 (In-Process OCI Image Acquisition)
- ADR-015 (Streaming OCI Layer Staging)
- `src/oci/cache/`
- `src/api/oci.rs`
