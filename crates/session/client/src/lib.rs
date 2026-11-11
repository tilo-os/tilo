//! A participant's client of the collaboration plane: the workspaces it takes part in, who is here
//! in each of them, and this device's own word on them.
//!
//! A host brings the transport; the client says what a participant says on it. What only one host
//! can do — a desktop's workspaces, channels, fabric and transfers — stays that host's.

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page runs on one thread, and what it keeps from JavaScript is never `Send` or `Sync`"
    )
)]

pub mod arrivals;
pub mod channels;
pub mod client;
pub mod content;
pub mod custody;
pub mod engine;
pub mod gate;
pub mod identity;
pub mod membership;
pub mod plane;
pub mod presence;

pub use arrivals::Arrivals;
pub use client::{Client, Presence};
pub use plane::{
    Cmd, Device, Event, Outbox, RosterDevice, SayPresence, Topic, Transport, VoiceEvent, topic_of,
};
pub use presence::OwnPresence;
pub use tilo_live::presence::Call as InCall;
pub use tilo_live::presence::{LiveCall, LiveCaller};

/// What became of an enrollment or an entry, and the workspace it was for.
#[derive(Debug)]
pub struct Enrolled {
    /// The slot the desk made for it.
    pub slot: tilo_desktop_model::WorkspaceSlot,
    pub workspace_id: tilo_desktop_model::WorkspaceId,
    /// Whether this device entered one of the person's own workspaces as their further device,
    /// rather than redeeming an invitation into someone's.
    pub member: bool,
    /// The name the person's list gave it, when it was entered from there.
    pub name: Option<String>,
    /// `Ok` once this device holds the workspace and takes part in it.
    pub result: Result<(), String>,
    /// The entry was for a workspace already on this desk.
    pub held: bool,
}
