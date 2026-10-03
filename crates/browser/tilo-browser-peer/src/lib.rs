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
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod carrier;
mod channels;
mod clock;
pub mod custody;
pub mod document;
pub mod health;
pub mod invite;
pub mod live;
#[cfg(feature = "net")]
pub mod net;
pub mod session;
#[cfg(feature = "toolkit")]
pub mod transport;
pub mod voice;

pub use carrier::{HttpClient, HttpFuture, HttpsLink, LinkError};
pub use custody::{
    CertifyError, IssuedRole, MemoryPeerStore, OutboxEntry, PeerRecord, PeerStore, certify,
    certify_with_root, issue_device_cert,
};
pub use document::{ContentCipher, DocumentRoute, OpenedDocument, coalesce};
pub use health::{PeerSnapshot, SessionHealth};
pub use live::{LiveChange, LiveState, RemoteMember};
#[cfg(feature = "net")]
pub use net::NetHttp;
pub use session::{
    Failure, FailureSubject, Host, LocalState, PAGE_FAILURE_CODES, PeerConfig, PeerEvent,
    PeerEvents, PeerHandle, PeerStatus, RoomsCompletion, TransportPhase,
};
pub use tilo_relay_client::{DeviceSigner, SignError, SignFuture};
pub use voice::VoiceEvent;
