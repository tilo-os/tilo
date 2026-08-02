//! qOS application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary
//! in `main.rs` is just argv parsing + orchestration.
//!
//! - [`sandbox`]      — bubblewrap argv construction.
//! - [`appdir`]       — the one directory an app may write to. The
//!   launcher picks it (per room, per package), creates it, and points
//!   `HOME`/`XDG_*` inside it. No manifest field feeds it, so no app can
//!   name a writable path and no app can opt out of per-room
//!   partitioning.
//! - [`profile`]      — the fixed set of host paths every app may read.
//!   Replaces the per-manifest `sandbox.fs.read` list: those entries were
//!   identical across the fleet and were a place a package named host
//!   paths.
//! - [`gpu`]          — which GPU (and therefore which EGL/Vulkan ICD)
//!   a launched app renders on. The compositor decides; this resolves
//!   its decision into loader manifests and a device to expose.
//! - [`landlock`]     — FS access plan + in-process restriction.
//! - [`seccomp`]      — baseline BPF policy.
//! - [`services`]     — well-known service socket resolution.
//! - [`capabilities`] — the workspace grants the user made, resolved
//!   into binds. The manifest is the packager's ceiling; this is the
//!   user's decision, and the sandbox exposes only what both allow.
//! - [`netgate`]      — per-host egress. A network grant naming a host
//!   leaves the namespace empty and brokers the app's traffic through a
//!   socket the launcher holds, so the named host is the only one
//!   reachable.
//! - [`run`]          — top-level `run` orchestration glue.
//!
//! When `qos-pkg` lands, the public surface of `manifest` is the only
//! piece that needs to change; the rest are consumers of plain data
//! types and continue working unmodified.

pub mod appdir;
pub mod capabilities;
pub mod dbus_proxy;
pub mod gpu;
pub mod hardening;
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
