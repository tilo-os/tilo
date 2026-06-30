//! qOS application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! Initial commit on this branch lands the manifest shim only; the
//! sandbox, Landlock, seccomp, services and orchestration modules
//! are added in subsequent commits.
//!
//! When `qos-pkg` lands, the public surface of `manifest` is the only
//! piece that needs to change; the rest are consumers of plain data
//! types and continue working unmodified.

pub mod manifest;
