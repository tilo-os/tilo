//! A member's client of the peer network: the workspaces it takes part in, who is here
//! in each of them, and this device's own word on them.
//!
//! A host brings the transport; the client says what a member says on it. What only one host
//! can do — a desktop's workspaces, channels, fabric and transfers — stays that host's.
//!
//! A page, an agent or a program is a device of its person through the server transport the client
//! carries itself: [`start_session`] keeps one certified server session to the person's host, held
//! through a [`Link`], and [`device::Device`] is the one entry a program takes it by. It carries
//! signed document envelopes ([`document`]) as acknowledged commits from a persisted outbox
//! replayed after every reconnect, each joined workspace's governance in its replica, the live
//! peer network's presence and previews, and calls ([`voice`]).

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        clippy::future_not_send,
        reason = "a page runs on one thread, and what it keeps from JavaScript is never `Send` or `Sync`"
    )
)]

pub mod arrivals;
pub mod client;
mod clock;
pub mod content;
/// A device's entry into its person's workspaces: the one the CLI, agents, the API server and the
/// page all take.
pub mod device;
pub mod engine;
pub mod entered;
pub mod health;
mod held_live;
pub mod identity;
pub mod link;
pub mod membership;
pub mod membership_check;
pub mod message_policy;
pub mod narration;
pub mod network;
pub mod personal;
pub mod presence;
mod replica;
/// This session's presence tracks, as a member's client says on them.
pub mod saying;
pub mod session;
pub mod states;
pub mod surfaces;
pub mod way_in;
pub mod workspace_keys;

pub use arrivals::Arrivals;
pub use client::{Client, Presence};
pub use health::{EntryHealth, PeerSnapshot, SessionHealth};
pub use link::{
    Entry, Events, Failure, FailureKind, Held, InboundActions, Link, LocalState,
    PAGE_FAILURE_CODES, PeerStatus, Reports, TransportPhase,
};
pub use membership_check::{MemoryPeerStore, PeerEntry, PeerStore};
pub use network::{
    Answer, CallFrame, CallPicture, CallShown, Closed, Cmd, Device, Event, Kept, Outbox, PageShown,
    Receipt, RecvTimeoutError, RosterDevice, SayPresence, Topic, Transport, TryRecvError, Waits,
    answer, topic_of,
};
pub use personal::PersonalWorkspaces;
pub use presence::OwnPresence;
pub use saying::saying;
#[cfg(not(target_arch = "wasm32"))]
pub use session::{Lent, start_session_lent};
pub use session::{PeerConfig, SessionMemberServer, member_server, start_session};
pub use states::{SyncState, WorkspaceState};
pub use tilo_live::presence::Call as InCall;
pub use tilo_live::presence::{LiveCall, LiveCaller};
/// The route a record is opened and written by: what [`Cmd`]s and [`Event`]s name.
pub use tilo_peer_state::document_frame::DocumentRoute;
pub use tilo_server_protocol::DocumentAnswer;

/// Why an enrollment or an entry gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollFailure {
    /// The host, the invitation or the workspace refused: nothing was joined.
    Refused(String),
    /// This device could not take the workspace in (an isolated Automerge worker crashed,
    /// overran its deadline or never started). Nothing says the join was refused: the workspace
    /// stays where the join made it, and the next join attempt proceeds.
    Device(String),
}
impl EnrollFailure {
    /// Whether this device failed, rather than the join being refused.
    #[must_use]
    pub fn is_device_failure(&self) -> bool {
        matches!(self, Self::Device(_))
    }
}
impl std::fmt::Display for EnrollFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Device(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for EnrollFailure {}
impl From<String> for EnrollFailure {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}
impl From<&str> for EnrollFailure {
    fn from(reason: &str) -> Self {
        Self::Refused(reason.into())
    }
}
impl From<tilo_peer_state::owner::InstallError> for EnrollFailure {
    fn from(error: tilo_peer_state::owner::InstallError) -> Self {
        if error.is_device_failure() {
            Self::Device(error.to_string())
        } else {
            Self::Refused(error.to_string())
        }
    }
}

/// What became of an enrollment or an entry, and the workspace it was for.
#[derive(Debug)]
pub struct Enrolled {
    pub workspace_id: tilo_desktop_model::WorkspaceId,
    /// Whether this device entered one of the person's own workspaces as their further device,
    /// rather than redeeming an invitation into someone's.
    pub member: bool,
    /// The name the person's list gave it, when it was entered from there.
    pub name: Option<String>,
    /// `Ok` once this device holds the workspace and takes part in it.
    pub result: Result<(), EnrollFailure>,
    /// The entry was for a workspace already on this layout.
    pub held: bool,
}
