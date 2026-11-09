//! The session as a client of the collaboration plane: the workspaces it shows, this person's
//! presence in them, and everyone else's.
//!
//! [`presence`] is what every host runs: the boards of each workspace and this device's own word on
//! them. What a host binds to reach the others — this desktop's peer actor on an iroh endpoint —
//! is `client`'s, and the page's transport replaces it.
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport is handed these boards next")
)]
pub(crate) mod presence;

pub use presence::OwnPresence;
pub use tilo_live::presence::Call as InCall;
/// A call in progress in a workspace, as the people in it say.
pub use tilo_live::presence::{LiveCall, LiveCaller};

#[cfg(target_os = "linux")]
mod client;
#[cfg(target_os = "linux")]
pub use client::{
    Client, Enrolled, Enrolling, Entering, Entry, JOIN_PATIENCE, PeerDocuments, configured_hosts,
    device_identity,
};
#[cfg(target_os = "linux")]
pub(crate) use client::{ReceivedSurface, SiblingDials};
