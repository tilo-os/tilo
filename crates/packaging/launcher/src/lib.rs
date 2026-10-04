//! Tilo application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! - [`sandbox`] — bubblewrap argv construction.
//! - [`appdir`] — the one directory an app may write to, per room and
//!   package; no manifest field feeds it.
//! - [`profile`] — the fixed set of host paths every app may read.
//! - [`gpu`] — which GPU a launched app renders on, resolved into
//!   loader manifests and a device to expose.
//! - [`landlock`] / [`landlock_shim`] — FS access planning plus fail-closed
//!   restriction inside the completed bwrap mount namespace.
//! - [`seccomp`] — baseline BPF policy.
//! - [`services`] — well-known service socket resolution.
//! - [`capabilities`] — workspace grants resolved into binds; the
//!   manifest is the packager's ceiling, this is the user's decision.
//! - [`netgate`] — per-host egress, brokered through a socket the
//!   launcher holds.
//! - [`run`] — top-level `run` orchestration glue.
//! - [`host`] — the one launch path with *no* sandbox (unpackaged
//!   apps); read its module docs before touching it.
//!
//! The launcher-facing manifest surface is kept in [`manifest`]; policy types
//! are shared with `tilo-app-package`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod appdir;
pub mod capabilities;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod dbus_proxy;
pub mod dev_env;
pub mod gpu;
pub mod hardening;
#[cfg(target_os = "linux")]
pub mod host;
#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod landlock_shim;
pub mod managed;
pub mod manifest;
pub mod netgate;
pub mod performance;
pub mod profile;
#[cfg(target_os = "linux")]
pub mod provider_sandbox;
#[cfg(target_os = "linux")]
pub mod run;
#[cfg(target_os = "linux")]
pub mod sandbox;
pub mod scope;
pub mod seccomp;
pub mod services;

#[cfg(test)]
mod managed_tests;
