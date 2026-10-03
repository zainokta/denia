# Managed Zot Registry Design

## Goal

Replace Denia's hosted OCI registry storage/service responsibility with a Denia-managed local Zot process, while preserving Denia's public `/v2` authentication and project RBAC boundary and retaining Denia's hardened rootfs materialization path.

## Scope

This change introduces Zot as a required managed host dependency for the hosted registry path. Denia setup installs a pinned Zot release when missing or outdated, verifies the downloaded binary by SHA-256, writes a loopback-only Zot configuration and systemd unit, starts Zot before Denia, and makes `denia.service` depend on it.

The first migration boundary is deliberately conservative:

- Zot owns the local hosted-registry blob/manifest storage and GC service.
- Denia remains the public authorization edge and keeps project/service RBAC semantics.
- Denia's external/private registry puller remains in place because its credentials are project-scoped and do not map safely to Zot's global upstream credential configuration.
- Denia's rootfs unpacker and runtime bundle materialization remain unchanged.

## Managed Zot contract

- Version: `2.1.20`.
- Binary: `/usr/local/bin/zot`.
- Config: `/etc/denia/zot.json`.
- Storage: `/var/lib/denia/zot`.
- Listen address: `127.0.0.1:5000` only.
- Service: `zot.service`, running as `denia:denia`.
- GC: enabled, delayed by 1 hour, periodic every 24 hours.
- Storage commits are enabled for durability.

Pinned release SHA-256 values:

- linux/amd64: `a32e42d042d1f17b5b1317e55cc1a415a744c873dcd05c25c56b665478258bcb`
- linux/arm64: `d6a39475587be18ec3d42e0d2bfa50f5c5064cbbcdc222dbd88e55ecf69dd8e9`

## Installation and lifecycle

`install.sh` remains build-only. Host provisioning already belongs to `sudo denia setup`, so setup is extended with idempotent Zot steps:

1. install or upgrade Zot to the pinned version;
2. create the Zot storage/config directories;
3. write and validate the config;
4. write `zot.service`;
5. `systemctl daemon-reload`;
6. enable/start Zot;
7. wait for Zot to become active;
8. enable/start Denia.

`denia.service` declares an ordering/dependency on `zot.service` so a host reboot follows the same order.

## Security

Zot never binds a public interface. Denia remains the only public registry endpoint. This preserves the existing Denia API-token authentication and project role enforcement, and prevents clients from bypassing Denia by connecting to Zot directly.

The Zot download uses HTTPS with TLS 1.2+ and a pinned SHA-256 digest. A mismatched binary is rejected before installation.

## Migration boundary

The existing external-image pull path and OCI unpacker are not replaced in this change. Project-specific private upstream credentials are intentionally not copied into Zot. A later migration can add a credential broker or another safe mapping before moving those pulls behind Zot.
