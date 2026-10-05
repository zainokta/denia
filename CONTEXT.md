# Denia

A single-node backend PaaS that builds and runs workloads with Denia-owned Linux runtime isolation.

## Language

### Disk reclamation

**GC**:
An automatic, periodic, retention-bound sweep of one store (the OCI layer cache or the hosted registry).
_Avoid_: cleanup, prune

**Prune**:
An operator-requested, previewed and confirmed reclamation across many stores at once, surfaced on the CLI as `denia clean`.
_Avoid_: GC, purge, wipe

**Rootfs bundle**:
The unpacked, content-addressed root filesystem materialized for one artifact digest.
_Avoid_: image, artifact dir

**Crash leftover**:
A temporary build, checkout, upload, or staged-rootfs directory that a normal run would have removed but an interrupted one left behind.
_Avoid_: orphan, temp files
