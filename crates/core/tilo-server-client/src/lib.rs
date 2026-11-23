//! `tilo-server-client` — the device side of the Tilo server + server that every
//! peer shares, native or in a browser.
//!
//! - [`frame`] — the length-prefixed frame codec every handshake and link uses;
//! - [`certify`] — transport signed certification requests, check responses,
//!   and carry approvals from root-holding devices and members; a device
//!   without a certificate asks for one with [`certify::certify`];
//! - [`certification`] — the certificates a person's root issues, and its
//!   answer to a pending pairing;
//! - [`pairing`] — the pairing ceremony a member's root answers, and the
//!   certificates a person's root issues;
//! - [`login`] — the pairing link, the signed-in device, and the workspace entry;
//! - [`sign_in`] — a device proving its person to a service that asks;
//! - [`invite`] — joining a workspace another server governs;
//! - [`signer`] — the device key as a signer, so a browser's
//!   non-extractable key can authenticate;
//! - [`error`] — why a call to a server failed, and why a server turned a device
//!   away;
//! - [`server`] — where a workspace's server is reached;
//! - [`link`] — how a device reaches it for each thing it asks: iroh
//!   natively, the browser's WebTransport in a page;
//! - [`https`] — what a page asks a server before it holds a session, over
//!   HTTPS through its `fetch`: the server's certificate and address, and
//!   joining a workspace.
//!
//! The messages themselves are defined in [`tilo_server_protocol`]. Without
//! the default `dial` feature the dialers (certification, the link) are left
//! out and the signer boundary, the co-edit payloads and the frame codec remain.

#![deny(rust_2018_idioms)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "a page's HTTP client answers through JavaScript, so the futures that await it stay on the page's one thread"
    )
)]

#[cfg(feature = "dial")]
pub mod certification;
#[cfg(feature = "dial")]
pub mod certify;
pub mod error;
pub mod frame;
pub mod host;
pub mod https;
pub mod invite;
/// How a device reaches its server for each thing it asks: iroh natively, the
/// browser's WebTransport in a page.
#[cfg(feature = "dial")]
pub mod link;
/// A computer signing in as one of the person's devices, and the two ways
/// into a workspace with it: through the site's entry, and by an invitation.
/// Reading a login code is a computer's work alone (a page writes the code,
/// never reads it), so that part of the module is native only; the ways in
/// are a page's too ([`login::Entry`]).
#[cfg(feature = "dial")]
pub mod login;
/// A server's login codes and the person's stand-in approver, for the tab and the computer alike.
pub mod pairing;
pub mod sign_in;
pub mod signer;

#[cfg(all(feature = "dial", not(target_arch = "wasm32")))]
pub use certify::{Approver, request_certification};
#[cfg(feature = "dial")]
pub use certify::{
    CERTIFICATION_WAIT_NS, CertificationOutcome, KeptAsks, MemberApprover, MemberServer,
    ask_member, request_certification_at,
};
pub use error::{CertifyError, LinkError};
pub use host::{Carrier, ServerAddress};
pub use https::{Answer, AnswerFuture, HttpClient, HttpFuture};
pub use pairing::revoke_device;
#[cfg(feature = "dial")]
pub use pairing::{decide_pairing, next_pairings, pending_pairings};
pub use sign_in::{encode, grant_holder, grant_label, key_client, sign_in_proof};
pub use signer::{DeviceSigner, MaybeSendSync, SignError, SignFuture};
