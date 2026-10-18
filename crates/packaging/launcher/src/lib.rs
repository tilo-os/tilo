//! Tilo application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! - [`sandbox`] — bubblewrap argv construction.
//! - [`appdir`] — the one directory an app may write to, per room and
//!   package; no manifest field feeds it.
//! - [`profile`] — the fixed set of host paths every app may read.
//! - [`sandbox_view`] — that set pre-built once as a read-only view, so a
//!   launch mounts it in a few binds instead of one per path.
//! - [`gpu`] — which GPU a launched app renders on, resolved into
//!   loader manifests and a device to expose.
//! - [`landlock`] / [`landlock_shim`] — FS access planning plus fail-closed
//!   restriction inside the completed bwrap mount namespace.
//! - [`loader_preflight`] — whether the sandboxed loader will find every
//!   library the app links against, checked before `bwrap` runs.
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

pub mod app_cgroup;
pub mod appdir;
#[cfg(target_os = "linux")]
mod browser_projection;
pub mod capabilities;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod dbus_proxy;
pub mod dev_env;
pub mod gpu;
pub mod hardening;
#[cfg(target_os = "linux")]
mod headless_console;
#[cfg(target_os = "linux")]
pub mod host;
#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod landlock_shim;
#[cfg(target_os = "linux")]
pub mod lease;
#[cfg(unix)]
pub mod loader_preflight;
pub mod managed;
pub mod manifest;
pub mod mesa_seed;
pub mod netgate;
#[cfg(target_os = "linux")]
pub mod ordinary_child;
#[cfg(target_os = "linux")]
pub mod ordinary_namespace;
#[cfg(target_os = "linux")]
pub mod owned_service;
pub mod performance;
pub mod priority;
pub mod profile;
#[cfg(target_os = "linux")]
pub mod provider_sandbox;
#[cfg(unix)]
pub mod registration_gate;
#[cfg(target_os = "linux")]
pub mod retained_view;
#[cfg(target_os = "linux")]
pub mod run;
#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(target_os = "linux")]
pub mod sandbox_view;
pub mod scope;
pub mod seccomp;
pub mod services;
#[cfg(target_os = "linux")]
pub mod spawner;
#[cfg(target_os = "linux")]
pub mod system_authority;
#[cfg(target_os = "linux")]
pub mod system_stats;

#[cfg(test)]
mod managed_tests;
