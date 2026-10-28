//! The typed system information service.
//!
//! An app never reads `/proc` or `/sys` of the machine: its sandbox has a
//! process namespace of its own, so it would see only itself, and opening
//! the host's would hand it every other program's command line and
//! environment. Instead the session (the compositor) reads the kernel and
//! pushes typed readings to the apps allowed to see them:
//!
//! - [`protocol`]: the wire contract and the [`protocol::Grant`]s that bound
//!   what a connection sees;
//! - [`probe`]: what is read from the kernel, and what never is;
//! - [`collect`]: counters into rates, and a reading cut to a grant;
//! - [`apps`]: which app a process belongs to (by its cgroup), and which
//!   processes the person may end;
//! - [`end`]: ending a process without racing pid reuse;
//! - [`hub`]: subscriptions, served by one thread that runs only while
//!   someone subscribes;
//! - [`storage`]: the machine's block devices and each drive's health,
//!   answered once per request;
//! - [`server`]: the session's socket, behind the launcher's handoff;
//! - [`client`]: the app's side.
//!
//! The launcher decides what an app is granted (`system-info` and
//! `processes` for the apps the image ships, whatever the room's
//! capabilities name for others) and writes it into the connection before
//! the app holds it; a grant the person gives later through the permission
//! prompt is added by the service. The architecture note is
//! `docs/architecture/system-settings-services.md`, "System information".

pub mod apps;
#[cfg(unix)]
pub mod client;
pub mod collect;
pub mod end;
pub mod hub;
pub mod probe;
pub mod protocol;
#[cfg(unix)]
pub mod server;
pub mod storage;

#[cfg(all(test, target_os = "linux"))]
mod tests;

pub use protocol::{Grant, Grants, SERVICE};
