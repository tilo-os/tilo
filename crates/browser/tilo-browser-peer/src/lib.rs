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
//! [`health`] derives `PeerSnapshot` and `SessionHealth` v1 from it. With the
//! `toolkit` feature, [`transport::RelayCollabTransport`] plugs the session
//! into `CollaborationManager::with_transport`.

#![deny(rust_2018_idioms)]

mod channels;
mod clock;
pub use neighbour_wire::content;
pub mod custody;
pub mod document;
pub mod health;
mod held_live;
pub mod invite;
pub mod live;
mod previews;
mod replica;
pub mod session;
pub mod surfaces;
#[cfg(feature = "toolkit")]
pub mod transport;
pub mod voice;

pub use channel_crypto::ChannelContentKeys;
pub use channels::{document_admission, document_history_admission};
pub use custody::{
    CertifyError, IssuedRole, MemoryPeerStore, OutboxEntry, PeerRecord, PeerStore, answer_pairing,
    certify, certify_with_root, decide_pairing, issue_device_cert, pending_pairings,
};
pub use health::{PeerSnapshot, SessionHealth};
pub use previews::{LiveChange, LiveState, RemoteMember};
pub use session::{
    CallFrame, CallPicture, CallShown, Completion, Failure, FailureSubject, Host, LocalState,
    NodeCompletion, PAGE_FAILURE_CODES, PeerConfig, PeerEvent, PeerEvents, PeerHandle, PeerStatus,
    RoomsCompletion, TransportPhase,
};
pub use tilo_live::live::Track;
pub use tilo_relay_client::https::{HttpClient, HttpFuture, HttpsLink, LinkError, host_info};
pub use tilo_relay_client::{DeviceSigner, SignError, SignFuture};
pub use voice::VoiceEvent;
