//! qOS application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! - [`sandbox`]   — bubblewrap argv construction.
//! - [`landlock`]  — FS access plan + in-process restriction.
//! - [`seccomp`]   — baseline BPF policy.
//! - [`services`]  — well-known service socket resolution.
//! - [`run`]       — top-level `run` orchestration glue.
//!
//! When `qos-pkg` lands, the public surface of `manifest` is the only
//! piece that needs to change; the rest are consumers of plain data
//! types and continue working unmodified.

pub mod landlock;
pub mod manifest;
pub mod run;
pub mod sandbox;
pub mod seccomp;
pub mod services;
