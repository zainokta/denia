// Module surface is split by build profile (ADR-030): `cli` (and its
// client submodules) compile everywhere; everything below depends on the
// Linux runtime, ingress, persistence, or host-provisioning stack and is
// gated behind the `server` feature so client-only builds stay cross-platform.
pub mod cli;

#[cfg(feature = "server")]
pub mod api;
#[cfg(feature = "server")]
pub mod app;
#[cfg(feature = "server")]
pub mod artifacts;
#[cfg(feature = "server")]
pub mod auth;
#[cfg(feature = "server")]
pub mod autoscale;
#[cfg(feature = "server")]
pub mod command;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod daemon;
#[cfg(feature = "server")]
pub mod deploy;
#[cfg(feature = "server")]
pub mod domain;
#[cfg(feature = "server")]
pub mod health;
#[cfg(feature = "server")]
pub mod ingress;
#[cfg(feature = "server")]
pub mod observability;
#[cfg(feature = "server")]
pub mod oci;
#[cfg(feature = "server")]
pub mod rate_limit;
#[cfg(feature = "server")]
pub mod repo;
#[cfg(feature = "server")]
pub mod runtime;
#[cfg(feature = "server")]
pub mod scheduler;
#[cfg(feature = "server")]
pub mod secrets;
#[cfg(feature = "server")]
pub mod state;
#[cfg(feature = "server")]
pub mod syscall;
#[cfg(feature = "server")]
pub mod verification;
#[cfg(feature = "server")]
pub mod web;
#[cfg(feature = "server")]
pub mod workload_launcher;

#[cfg(feature = "server")]
pub use ingress::socket_proxy;
#[cfg(feature = "server")]
pub use observability::{access_log, logs, metrics, node_metrics};
