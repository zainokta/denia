//! Cross-platform client command surface (ADR-030). These modules are kept
//! dependency-light so a client-only build avoids the Linux runtime, Pingora,
//! systemd, cgroup, and syscall code paths.

pub mod git;
pub mod http;
pub mod manifest;
pub mod profile;
