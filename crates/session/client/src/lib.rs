//! A participant's client of the collaboration plane: the workspaces it takes part in, who is here
//! in each of them, and this device's own word on them.
//!
//! A host brings the transport; the client says what a participant says on it. What only one host
//! can do — a desktop's workspaces, channels, fabric and transfers — stays that host's.
//!
//! A page, an agent or a program is a device of its person through the relay transport the client
//! carries itself: [`start_session`] keeps one certified relay session to the person's host, held
//! through a [`Link`], and [`device::Device`] is the one entry a program takes it by. It carries
//! signed document envelopes ([`document`]) as acknowledged commits from a persisted outbox
//! replayed after every reconnect, each joined workspace's governance in its replica, the live
//! plane's presence and previews, and calls ([`voice`]).

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        clippy::future_not_send,
        reason = "a page runs on one thread, and what it keeps from JavaScript is never `Send` or `Sync`"
    )
)]

pub mod arrivals;
pub mod channels;
pub mod client;
mod clock;
pub mod content;
pub mod custody;
/// A device's entry into its person's workspaces: the one the CLI, agents, the front door and the
/// page all take.
pub mod device;
pub mod document;
pub mod engine;
pub mod entered;
pub mod gate;
pub mod health;
mod held_live;
pub mod identity;
pub mod link;
pub mod membership;
pub mod narration;
pub mod plane;
pub mod presence;
mod replica;
/// This session's presence tracks, as a participant's client says on them.
pub mod saying;
pub mod session;
pub mod surfaces;
pub mod voice;
pub mod way_in;

pub use arrivals::Arrivals;
pub use client::{Client, Presence};
pub use custody::{MemoryPeerStore, PeerRecord, PeerStore};
pub use health::{EntryHealth, PeerSnapshot, SessionHealth};
pub use link::{
    Entry, Events, Failure, FailureSubject, Link, LocalState, PAGE_FAILURE_CODES, PeerStatus,
    Reports, TransportPhase,
};
pub use plane::{
    Answer, CallFrame, CallPicture, CallShown, Closed, Cmd, Device, Event, Outbox, PageShown,
    Receipt, RecvTimeoutError, RosterDevice, SayPresence, Topic, Transport, TryRecvError,
    VoiceEvent, answer, topic_of,
};
pub use presence::OwnPresence;
pub use saying::saying;
pub use session::{PeerConfig, SessionMemberHost, member_host, start_session};
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
