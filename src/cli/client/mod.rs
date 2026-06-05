//! Cross-platform client subcommands that talk to a remote Denia control plane
//! over the management API (distinct from the operator host-provisioning
//! commands). See ADR-033.

pub mod auth;
pub mod console;
pub mod create;
pub mod http;
pub mod init;
pub mod manifest;
pub mod pack;
pub mod profile;
pub mod push;
