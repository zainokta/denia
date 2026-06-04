//! Denia is a Linux single-node PaaS. The daemon, runtime isolation, ingress,
//! persistence, and management API all link Linux-only code (rustix `mount`,
//! pingora/boringssl, bundled rusqlite) and are gated to `target_os = "linux"`.
//!
//! The `cli` module's client subcommands (`auth`, `push`, `console`) are
//! portable, so the single `denia` crate also builds on macOS and Windows,
//! where only those client commands compile. See ADR-036.

// Portable: the client command surface and the shared types it uses. Compiles
// on every target.
pub mod cli;

// Linux-only server: everything that links the runtime/ingress/persistence
// stack. Absent on macOS/Windows so the client build never pulls it in.
#[cfg(target_os = "linux")]
pub mod api;
#[cfg(target_os = "linux")]
pub mod app;
#[cfg(target_os = "linux")]
pub mod artifacts;
#[cfg(target_os = "linux")]
pub mod auth;
#[cfg(target_os = "linux")]
pub mod autoscale;
#[cfg(target_os = "linux")]
pub mod command;
#[cfg(target_os = "linux")]
pub mod config;
#[cfg(target_os = "linux")]
pub mod daemon;
#[cfg(target_os = "linux")]
pub mod deploy;
#[cfg(target_os = "linux")]
pub mod domain;
#[cfg(target_os = "linux")]
pub mod health;
#[cfg(target_os = "linux")]
pub mod ingress;
#[cfg(target_os = "linux")]
pub mod observability;
#[cfg(target_os = "linux")]
pub mod oci;
#[cfg(target_os = "linux")]
pub mod rate_limit;
#[cfg(target_os = "linux")]
pub mod registry;
#[cfg(target_os = "linux")]
pub mod repo;
#[cfg(target_os = "linux")]
pub mod runtime;
#[cfg(target_os = "linux")]
pub mod scheduler;
#[cfg(target_os = "linux")]
pub mod secrets;
#[cfg(target_os = "linux")]
pub mod state;
#[cfg(target_os = "linux")]
pub mod syscall;
#[cfg(target_os = "linux")]
pub mod verification;
#[cfg(target_os = "linux")]
pub mod web;
#[cfg(target_os = "linux")]
pub mod workload_launcher;

#[cfg(target_os = "linux")]
pub use ingress::socket_proxy;
#[cfg(target_os = "linux")]
pub use observability::{access_log, logs, metrics, node_metrics};
