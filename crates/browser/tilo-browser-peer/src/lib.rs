//! `tilo-browser-peer` — the browser as a real Tilo peer.
//!
//! A paired page holds a device key. [`custody::certify`] asks the user's
//! host for a root-signed `DeviceCert`, which a root-holding device issues
//! through the host. An invited person instead uses [`invite`] to certify
//! their device with their own root and obtain governed workspace access.
//! [`session::PeerHandle`] keeps one certified relay session to that host:
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
//! [`presence_anchors`] carries an editor's ranges on the live plane as every
//! native device does.

#![deny(rust_2018_idioms)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        clippy::arc_with_non_send_sync,
        reason = "a page runs on one thread, and its signer, HTTP and store handles to JavaScript are never `Send`"
    )
)]

mod channels;
mod clock;
pub mod custody;
pub mod document;
pub mod health;
mod held_live;
pub mod invite;
/// Logging a computer in as one of the person's devices (native only).
#[cfg(not(target_arch = "wasm32"))]
pub mod login;
pub mod narration;
/// The login code a page hands a computer, read (native only: a page writes
/// it, never reads it).
#[cfg(not(target_arch = "wasm32"))]
pub mod pairing_link;
pub mod presence_anchors;
mod previews;
mod replica;
pub mod session;
pub mod session_actions;
pub mod sign_in;
pub mod surfaces;
pub mod voice;

pub use channels::{document_admission, document_history_admission};
pub use custody::{
    CertifyError, IssuedRole, MemoryPeerStore, OutboxEntry, PeerRecord, PeerStore, answer_pairing,
    certify, certify_with_root, decide_pairing, issue_agent_grant, issue_device_cert,
    issue_stand_in, pending_pairings, revoke_device,
};
pub use health::{PeerSnapshot, SessionHealth};
pub use previews::{LiveChange, LiveState, RemoteMember};
pub use session::{
    CallFrame, CallPicture, CallShown, Completion, Failure, FailureSubject, Host, LocalState,
    PAGE_FAILURE_CODES, PeerConfig, PeerEvent, PeerEvents, PeerHandle, PeerStatus, RoomsCompletion,
    TransportPhase,
};
pub use sign_in::{encode, grant_label, sign_in_proof};
/// The record a session's [`PeerHandle::open_route`] takes part in.
pub use tilo_peer_state::document_frame::DocumentRoute;
pub use tilo_relay_client::LinkError;
pub use tilo_relay_client::https::{HttpClient, HttpFuture, host_info};
pub use tilo_relay_client::{DeviceSigner, SignError, SignFuture};
pub use voice::VoiceEvent;
