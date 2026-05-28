# ADR-021: Control-Plane SOPS Secret Encryption

- Status: Accepted
- Date: 2026-05-28
- Supersedes (in part): operator-managed `.sops.yaml` workflow for registry credentials.

## Context

Until now, Denia only *decrypts* SOPS-encrypted files at deploy time; operators
must place `secrets/<project_id>/<ref>.sops.yaml` on disk out-of-band and POST
only an opaque `secret_ref` string to the API. This breaks UX: the web console
asks operators to type a "secret reference" with no way to enter the actual
credential. Frontend users have repeatedly sent the credential payload itself
in the `secret_ref` field, which then fails validation.

## Decision

The control plane owns SOPS encryption for registry credentials:

1. Add `DENIA_AGE_RECIPIENT` env (age public key). `denia` refuses to start
   when registry creation is attempted without it.
2. `POST /v1/projects/{pid}/registries` and `PATCH …/{rid}` accept the raw
   payload (`username`/`password` or `token`) instead of `secret_ref`.
3. The handler generates a `SecretRef` deterministically from a UUIDv7,
   encrypts the payload with `sops --encrypt --age $RECIPIENT --input-type
   json --output-type yaml`, and writes
   `<data_dir>/secrets/<project_id>/<ref>.sops.yaml` with mode `0600`.
4. The previously documented operator-managed `.sops.yaml` flow is retired
   for registry credentials. Existing service-secret refs (SSH deploy keys,
   etc.) remain operator-managed for now; their migration is out of scope.

## Consequences

- Easier: end-to-end UX from web console; no out-of-band file shuffling.
- Easier: per-project namespacing remains by construction.
- Harder: control plane now needs filesystem write access to `secrets/`
  (already true for data_dir). Plaintext briefly transits the `sops` binary;
  payload is written to a `0600` temp file in the same secrets dir before
  `sops --encrypt` is invoked, then deleted.
- Harder: bootstrap docs must instruct operators to set both
  `DENIA_AGE_RECIPIENT` (encryption) and `SOPS_AGE_KEY_FILE` (decryption).

## Alternatives Considered

- Derive recipient from `SOPS_AGE_KEY_FILE` at boot — rejected: adds an `age`
  crate dependency just for public-key derivation.
- `.sops.yaml` creation rules — rejected: operator still has to manage a
  separate config file; not simpler than one env var.
- Frontend pre-creates credential then references it — rejected: doubles
  the API surface, still requires backend encryption.

## References

- [ADR-001 Initial Backend Architecture](001-initial-backend-architecture.md)
- [`src/secrets.rs`](../../src/secrets.rs)
