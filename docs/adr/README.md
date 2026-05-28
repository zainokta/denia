# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records for Denia.

## Format

Each ADR should include:

- **Status**: Proposed, Accepted, Deprecated, or Superseded
- **Date**: YYYY-MM-DD
- **Context**: Why this decision is needed
- **Decision**: What Denia will do
- **Consequences**: What becomes easier or harder
- **Alternatives Considered**: Other options and why they were rejected
- **References**: Relevant docs or prior decisions

## Index

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| [001](001-initial-backend-architecture.md) | Initial Backend Architecture | Accepted | 2026-05-24 |
| [002](002-frontend-effect-logic-layer.md) | Frontend Effect Logic Layer | Proposed | 2026-05-24 |
| [003](003-linux-runtime-process-runner.md) | Linux Runtime Process Runner | Accepted | 2026-05-24 |
| [004](004-embed-web-console.md) | Embed Web Console in Service Binary | Proposed | 2026-05-24 |
| [005](005-runtime-security-hardening.md) | Runtime Security Hardening | Accepted | 2026-05-25 |
| [006](006-projects-and-migrations.md) | Projects And Versioned Migrations | Proposed | 2026-05-25 |
| [007](007-ingress-tls.md) | Ingress + TLS | Proposed | 2026-05-25 |
| [008](008-rbac.md) | Project-Scoped RBAC | Proposed | 2026-05-25 |
| [009](009-observability.md) | Observability (Node, Workloads, Access Log) | Proposed | 2026-05-25 |
| [010](010-jobs-scheduler.md) | Jobs and Scheduler | Proposed | 2026-05-25 |
| [011](011-inprocess-oci-acquisition.md) | In-Process OCI Image Acquisition | Proposed | 2026-05-25 |
| [012](012-src-modularization.md) | src/ Modularization and Per-Aggregate Repositories | Proposed | 2026-05-25 |
| [013](013-domain-verification.md) | Domain Support With HTTP File Verification | Accepted | 2026-05-25 |
| [014](014-per-service-registry.md) | Per-Service OCI Registry Configuration | Proposed | 2026-05-26 |
| [015](015-streaming-oci-layer-staging.md) | Streaming OCI Layer Staging | Proposed | 2026-05-27 |
| [016](016-managed-traefik.md) | Denia-Managed Traefik | Superseded by ADR-020 | 2026-05-27 |
| [017](017-service-crud-api.md) | Service CRUD API | Proposed | 2026-05-27 |
| [018](018-autoscaling.md) | Autoscaling | Accepted | 2026-05-27 |
| [019](019-runtime-filesystem-isolation.md) | Per-Replica Runtime Filesystem Isolation | Accepted | 2026-05-27 |
| [020](020-pingora-ingress.md) | In-Process Pingora Ingress | Accepted | 2026-05-28 |
| [021](021-oci-layer-cache.md) | Persistent OCI Layer Cache With Weekly GC | Accepted | 2026-05-28 |

## Contributing

Create or update an ADR for changes to runtime isolation, ingress, secret handling, persistence, API contracts, dependency choices, or deployment architecture.
