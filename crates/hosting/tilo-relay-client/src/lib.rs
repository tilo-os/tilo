//! `tilo-relay-client` — the device side of the Tilo host + relay that every
//! peer shares, native or in a browser.
//!
//! - the session with a relay over iroh is `tilo_relay_session::RelayClient`;
//! - [`frame`] — the length-prefixed frame codec every relay stream uses.
//!   The daemon and the relay session client in `tilo-relay-session` read and write
//!   through the same functions;
//! - [`coedit`] — the co-edit payloads carried through the host; the iroh
//!   session speaking them is `tilo_relay_session::coedit::RelayCoeditSession`;
//! - [`certify`] — ask a host for a `DeviceCert`, or approve such requests
//!   as a root-holding device or as a member's root;
//! - [`pairing`] — the pairing ceremony a member's root answers, and the
//!   certificates a person's root issues;
//! - [`login`] — a computer signing in as one of the person's devices, the
//!   login code it reads, and the two ways into the workspace it names;
//! - [`sign_in`] — a device proving its person to a service that asks;
//! - [`invite`] — joining a workspace another host governs, with no session
//!   there;
//! - [`signer`] — the device key as a signer, so a browser's
//!   non-extractable key can authenticate;
//! - [`error`] — why a call to a host failed, and why a host turned a device
//!   away;
//! - [`host`] — where a workspace's host is reached;
//! - [`https`] — what a page asks a host before it holds a session, over
//!   HTTPS through its `fetch`: the host's certificate and address, and
//!   joining a workspace.
//!
//! The messages themselves are defined in [`tilo_relay_protocol`]. Without
//! the default `iroh` feature certification is left out and the signer seam,
//! the co-edit payloads and the frame codec remain, for peers that reach the host another way (a
//! browser over HTTPS).

#![deny(rust_2018_idioms)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "a page's HTTP client answers through JavaScript, so the futures that await it stay on the page's one thread"
    )
)]

#[cfg(feature = "iroh")]
pub mod certify;
pub mod coedit;
pub mod error;
pub mod frame;
pub mod host;
pub mod https;
pub mod invite;
/// A computer signing in as one of the person's devices, and the two ways
/// into a workspace with it: through the site's door, and by an invitation.
/// Reading a login code is a computer's work alone (a page writes the code,
/// never reads it), so that part of the module is native only; the ways in
/// are a page's too ([`login::Door`], [`login::redeem`]).
#[cfg(feature = "iroh")]
pub mod login;
/// A host's login codes and the person's stand-in approver, for the tab and the computer alike.
pub mod login_code;
pub mod pairing;
#[cfg(feature = "iroh")]
pub mod questions;
pub mod sign_in;
pub mod signer;

#[cfg(feature = "iroh")]
pub use certify::{
    Approver, CERTIFICATION_WAIT_NS, CertificationOutcome, MemberApprover, certify,
    request_certification,
};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound};
pub use error::{CertifyError, LinkError};
pub use host::{Carrier, Host};
pub use https::{Answer, AnswerFuture, HttpClient, HttpFuture};
pub use pairing::{
    IssuedRole, NODE_CERT_LIFETIME_NS, certify_with_root, issue_agent_grant, issue_device_cert,
    issue_stand_in, revoke_device,
};
#[cfg(feature = "iroh")]
pub use pairing::{answer_pairing, decide_pairing, pending_pairings};
pub use sign_in::{encode, grant_label, sign_in_proof};
pub use signer::{DeviceSigner, MaybeSendSync, SignError, SignFuture};
pub use tilo_relay_protocol::transport::{ContentPeerRegistration, Envelope};
