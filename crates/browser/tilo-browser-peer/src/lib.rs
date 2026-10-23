//! `tilo-browser-peer` — the browser as a real Tilo peer.
//!
//! A paired page holds a device key. [`custody::certify`] asks the user's
//! host for a root-signed `DeviceCert`, which a root-holding device issues
//! through the host. An invited person instead uses [`invite`] to certify
//! their device with their own root and obtain governed room access.
//! [`session::PeerHandle`] keeps one certified relay session to that host:
//!
//! - the **durable plane**: signed document envelopes ([`document`]) sent as
//!   acknowledged commits from a persisted outbox and replayed after every
//!   reconnect;
//! - the **live plane** ([`live`]): ephemeral presence and previews relayed
//!   by the host to live members;
//! - **calls** ([`voice`]): the device's participation in the calls the host
//!   governs, and the grants for its voice carrier.
//!
//! [`health`] derives `PeerSnapshot` and `SessionHealth` v1 from it. With the
//! `toolkit` feature, [`transport::RelayCollabTransport`] plugs the session
//! into `CollaborationManager::with_transport`.
//!
//! See `docs/architecture/browser-runtime.md`, "Transport from browser to
//! machine".

#![deny(rust_2018_idioms)]

mod channels;
mod clock;
pub mod content;
pub mod custody;
pub mod document;
pub mod health;
mod held_live;
pub mod invite;
pub mod live;
pub mod remote_page;
mod room_layout;
pub mod session;
#[cfg(feature = "toolkit")]
pub mod transport;
pub mod voice;
mod watching;

pub use channel_crypto::ChannelContentKeys;
pub use channels::document_admission;
pub use custody::{
    CertifyError, IssuedRole, MemoryPeerStore, OutboxEntry, PeerRecord, PeerStore, answer_pairing,
    certify, certify_with_root, decide_pairing, issue_device_cert, pending_pairings,
};
pub use document::{ContentCipher, DocumentRoute, OpenedDocument, claimed_changes, coalesce};
pub use health::{PeerSnapshot, SessionHealth};
pub use live::{LiveChange, LiveState, RemoteMember};
pub use session::{
    Failure, FailureSubject, Host, LocalState, NodeCompletion, PAGE_FAILURE_CODES, PeerConfig,
    PeerEvent, PeerEvents, PeerHandle, PeerStatus, RoomsCompletion, TransportPhase,
};
pub use tilo_relay_client::https::{HttpClient, HttpFuture, HttpsLink, LinkError, host_info};
pub use tilo_relay_client::{DeviceSigner, SignError, SignFuture};
pub use voice::VoiceEvent;
