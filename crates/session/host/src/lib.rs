//! `tilo-session-host` — a participant's host of a workspace: the one way a
//! page, an agent or a program is a device of its person in a workspace.
//!
//! A device holds a device key, which [`tilo_relay_client::certify`] has the
//! person's host ask a root-holding device to certify, and
//! [`tilo_relay_client::invite`] certifies with a joining person's own root.
//! [`session::PeerHandle`] keeps one certified relay session to that host, and
//! [`device::Device`] is the one entry a program takes it by:
//!
//! - the **durable plane**: signed document envelopes ([`document`]) sent as
//!   acknowledged commits from a persisted outbox and replayed after every
//!   reconnect;
//! - the **workspace's governance**: each joined workspace's replica, built
//!   from the bootstrap the host sends ahead of the workspace's replay and
//!   admitting the governance it relays, as a native device's does (`replica`);
//! - the **live plane** ([`live`]): ephemeral presence and previews relayed
//!   by the host to live members;
//! - **calls** ([`voice`] for the relay's own media path, and the plane's
//!   camera and screen tracks for what a call carries): the grants that admit
//!   the page to a call, and the pictures and voice it publishes and shows.
//!
//! [`health`] derives `PeerSnapshot` and `SessionHealth` v1 from it.
//!
//! The identity layer's device side is [`tilo_relay_client`]: this crate enters
//! the workspace a signed-in computer's link names ([`device::Device::enter`])
//! and keeps the session.

#![deny(rust_2018_idioms)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        clippy::arc_with_non_send_sync,
        reason = "a page runs on one thread, and its signer, HTTP and store handles to JavaScript are never `Send`"
    )
)]

mod clock;
pub mod custody;
/// A device's entry into its person's workspaces: the one the CLI, agents, the front door and the
/// page all take.
pub mod device;
pub mod document;
pub mod entered;
pub mod health;
mod held_live;
pub mod narration;
mod replica;
/// This session's presence tracks, as a participant's client says on them.
pub mod saying;
pub use saying::saying;
pub mod session;
/// Another device's actions, asked through this device's session.
pub use tilo_session_core::remote as session_actions;
pub mod surfaces;
pub mod voice;
pub mod way_in;

pub use custody::{MemoryPeerStore, OutboxEntry, PeerRecord, PeerStore};
pub use document::{document_admission, document_history_admission};
pub use health::{PeerSnapshot, SessionHealth};
pub use session::{
    CallFrame, CallPicture, CallShown, Completion, Failure, FailureSubject, LocalState,
    PAGE_FAILURE_CODES, PeerConfig, PeerEvent, PeerEvents, PeerHandle, PeerStatus, RoomsCompletion,
    TransportPhase,
};
/// The record a session's [`PeerHandle::open_route`] takes part in.
pub use tilo_peer_state::document_frame::DocumentRoute;
/// The identity layer's device side, re-exported for this crate's own tests
/// and its peers.
pub use tilo_relay_client::https::{HttpClient, HttpFuture, host_info};
pub use tilo_relay_client::{
    Carrier, CertifyError, DeviceSigner, Host, IssuedRole, LinkError, MemberApprover, SignError,
    SignFuture, answer_pairing, certify, certify_with_root, decide_pairing, invite,
    issue_agent_grant, issue_device_cert, issue_stand_in, pending_pairings, revoke_device,
};
pub use voice::VoiceEvent;
