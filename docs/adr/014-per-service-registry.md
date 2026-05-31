# ADR-014: Per-Service OCI Registry Configuration

- **Status**: Proposed
- **Date**: 2026-05-26

## Context

ADR-011 landed in-process OCI image acquisition via `RegistryImagePuller` and a
single process-wide `StaticCredentialProvider`, while ECR and GAR support sat
behind cargo features (`ecr`, `gar`) that consumed credentials from
`DENIA_ECR_PASSWORD` / `DENIA_GAR_ACCESS_TOKEN` environment variables. That
model could not represent more than one private registry per process and tied
ECR/GAR rotation to operator-controlled env vars.

Workloads need per-service control over which registry to authenticate against
and how. The TODO captured this as item 16 ("OCI registry should be able to be
configured on each service").

## Decision

- Introduce a project-scoped `Registry` domain entity:

  ```rust
  pub struct Registry {
      pub id: Uuid,
      pub project_id: Uuid,
      pub name: String,
      pub endpoint: String,
      pub auth_kind: RegistryAuthKind,
      pub credential_ref: Option<SecretRef>,
  }

  pub enum RegistryAuthKind { Anonymous, Basic, Token, EcrToken, GarToken }
  ```

  Persisted as JSON in a new `registries` SQLite table (migration v6), unique
  per `(project_id, name)`. Credentials are referenced by `SecretRef` only —
  the API never returns decrypted payloads.

- Extend `ExternalImageSource` with two optional fields kept alongside the
  existing `image: String` for backwards compatibility:

  ```rust
  pub struct ExternalImageSource {
      pub image: String,
      pub credential: Option<SecretRef>,
      pub registry_id: Option<Uuid>,
      pub image_ref: Option<String>,
  }
  ```

  Validation accepts exactly one of (legacy `image`) or (new `registry_id` +
  `image_ref`); partial states are rejected.

- Replace the `RegistryCredentialProvider` trait + `StaticCredentialProvider`
  with a free function `resolve_registry_auth(kind, payload) -> RegistryAuth`
  in `src/oci/credentials.rs`. The `OciImagePuller::pull` trait signature
  changes to `pull(image, auth)`; auth is resolved at the deploy boundary,
  not inside the puller. `RegistryAuth` (re-exported from `oci_client`) is
  passed explicitly through the acquirer to each pull.

- Auth kinds map to `RegistryAuth` as follows:
  - `Anonymous` → `RegistryAuth::Anonymous`
  - `Basic` → split `"user:password"` payload → `Basic(user, pass)`
  - `Token` → `Bearer(token)`
  - `EcrToken` → `Basic("AWS", token)`
  - `GarToken` → `Basic("oauth2accesstoken", token)`

  ECR/GAR use pre-minted, short-lived tokens stored in SOPS (operator rotates
  externally via `aws ecr get-login-password` / `gcloud auth print-access-token`).
  No AWS or GCP SDK dependency is introduced.

- `deploy_external_image_source` resolves auth before calling the acquirer:
  load the `Registry`, decrypt the optional `credential_ref` via
  `SopsSecretStore`, run `resolve_registry_auth`, compose
  `{endpoint}/{image_ref}`, then pull. The legacy `image` path keeps working
  only for anonymous pulls. `ExternalImageSource.credential` is no longer
  accepted for new service writes and deploy fails closed if an existing row
  still contains it; authenticated image pulls must use a project `Registry`.

- Add admin-only CRUD endpoints under
  `/v1/projects/{project_id}/registries`. Deleting a registry that any
  service in the project references returns `StateError::RegistryInUse`
  (HTTP 409).

- Remove `src/oci/ecr.rs`, `src/oci/gar.rs`, the `ecr` / `gar` cargo
  features, and the `DENIA_ECR_PASSWORD` / `DENIA_GAR_ACCESS_TOKEN`
  environment variables. The same registries are now supported via the
  general SOPS-backed `Registry` record.

## Consequences

- Operators manage registries per project. Multiple private registries can
  coexist; each service points at exactly one (or stays on the legacy
  full-image anonymous path).
- ECR / GAR tokens are short-lived. Operators must rotate them via an
  external hook (cron, deploy script). In-process SigV4 / metadata-server
  exchange remains deferred — adding `aws-sdk-ecr` / `gcp_auth` is a
  separate ADR.
- `OciImagePuller::pull` is a breaking trait change; all implementations
  (production + fakes) updated in the same commit.
- API responses carry `SecretRef` names only. Decrypted payloads are scoped
  to the deploy call and dropped after `puller.pull` returns.

## Alternatives Considered

- **Credential-only override on the service** (no Registry entity): rejected
  because it duplicates endpoint + auth-kind across services in the same
  registry and offers no path for ECR/GAR.
- **System-scoped registries** shared across all projects: rejected for
  weaker tenancy. Project scope matches RBAC and the secrets model.
- **In-process SigV4 / GCP metadata exchange** for ECR / GAR: deferred —
  larger dependency surface, separate ADR-011 follow-up.
- **Strict migration** that requires every service to specify `registry_id`
  on upgrade: rejected. Dual-field keeps existing services deploying.

## References

- `docs/superpowers/specs/2026-05-25-per-service-oci-registry-design.md`
- `docs/superpowers/plans/2026-05-26-per-service-oci-registry.md`
- Amends ADR-011 (In-Process OCI Image Acquisition).
