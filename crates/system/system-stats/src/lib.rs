//! The typed system information service.
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
