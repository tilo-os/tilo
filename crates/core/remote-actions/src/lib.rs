//! The parts of the remote action model that only the hosts and processes using them need:
//! tilo-actions keeps the types every crate that names an action reads, and this crate holds
//! the machinery beside them, so a change to it rebuilds its users and not the whole tree.
//!
//! - [`asked`]: the asked records a host keeps, and the one place a person's answer to one is
//!   admitted.
//! - [`signing_in`]: an app's own reading of the workspace's accounts and their secrets, over its
//!   connection to the device's dispatch.

pub mod asked;
/// An app's own reading of the workspace's accounts and their secrets, over its connection to
/// the device's dispatch.
#[cfg(not(target_arch = "wasm32"))]
pub mod signing_in;
