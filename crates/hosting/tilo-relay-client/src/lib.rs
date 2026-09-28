//! `tilo-relay-client` — the device side of the Tilo host + relay that every
//! peer shares, native or in a browser.
//!
//! - [`client`] — [`RelayClient`], a device's v1 session with a relay over
//!   iroh (native peers and host agents); the relay's own v2 client, with
//!   durable actions and interactive owners, is `tilo_relay::client`;
//! - [`frame`] — the length-prefixed frame codec every relay stream uses.
//!   The daemon and the relay session client in `tilo-relay` read and write
//!   through the same functions.
//! - [`coedit`] — the co-edit payloads carried through the host; the iroh
//!   session speaking them is `tilo_relay::coedit::RelayCoeditSession`;
//! - [`certify`] — ask a host for a `DeviceCert`, or approve such requests
//!   as a root-holding device;
//! - [`signer`] — the device key as a signer, so a browser's
//!   non-extractable key can authenticate.
//!
//! The messages themselves are defined in [`tilo_relay_protocol`]. Without
//! the default `iroh` feature certification is left out and the signer seam,
//! the co-edit payloads and the frame codec remain, for peers that reach the host another way (a
//! browser over HTTPS).

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

#[cfg(feature = "iroh")]
pub mod certify;
#[cfg(feature = "iroh")]
pub mod client;
#[cfg(feature = "iroh")]
mod clock;
pub mod coedit;
pub mod frame;
pub mod signer;

#[cfg(feature = "iroh")]
pub use certify::{Approver, CertificationOutcome, CertifyError, request_certification};
#[cfg(feature = "iroh")]
pub use client::{ClientError, RelayClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound};
pub use signer::{DeviceSigner, MaybeSendSync, SignError, SignFuture};
pub use tilo_relay_protocol::transport::{ContentPeerRegistration, Envelope};
