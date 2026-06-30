//! qOS application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! - [`sandbox`]   — bubblewrap argv construction.
//! - [`landlock`]  — FS access plan + in-process restriction.
//!
//! Further modules (seccomp, services, run orchestration) ship in
//! subsequent commits on this branch.
//!
//! When `qos-pkg` lands, the public surface of `manifest` is the only
//! piece that needs to change; the rest are consumers of plain data
//! types and continue working unmodified.

pub mod landlock;
pub mod manifest;
pub mod sandbox;
