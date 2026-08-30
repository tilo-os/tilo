//! qOS application launcher library surface.
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
//! - [`landlock`] — FS access plan + in-process restriction.
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
//! When `qos-pkg` lands, the public surface of `manifest` is the only
//! piece that needs to change.

pub mod appdir;
pub mod capabilities;
pub mod dbus_proxy;
pub mod gpu;
pub mod hardening;
pub mod host;
pub mod landlock;
pub mod landlock_shim;
pub mod manifest;
pub mod netgate;
pub mod profile;
pub mod registry;
pub mod run;
pub mod sandbox;
pub mod seccomp;
pub mod services;
