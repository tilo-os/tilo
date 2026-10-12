//! `tilo-relay-client` — the device side of the Tilo host + relay that every
//! peer shares, native or in a browser.
//!
//! - the session with a relay over iroh is `tilo_relay_session::RelayClient`;
//! - [`frame`] — the length-prefixed frame codec every relay stream uses.
//!   The daemon and the relay session client in `tilo-relay-session` read and write
//!   through the same functions.
//! - [`coedit`] — the co-edit payloads carried through the host; the iroh
//!   session speaking them is `tilo_relay_session::coedit::RelayCoeditSession`;
//! - [`certify`] — ask a host for a `DeviceCert`, or approve such requests
//!   as a root-holding device;
//! - [`environment`] — a terminal into an environment session on a relay
//!   ([`open_remote_environment`], [`PtyStream`]);
//! - [`signer`] — the device key as a signer, so a browser's
//!   non-extractable key can authenticate.
//!
//! The messages themselves are defined in [`tilo_relay_protocol`]. Without
//! the default `iroh` feature certification is left out and the signer seam,
//! the co-edit payloads and the frame codec remain, for peers that reach the host another way (a
//! browser over HTTPS).

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

#[cfg(feature = "iroh")]
pub mod certify;
pub mod coedit;
#[cfg(feature = "iroh")]
pub mod environment;
pub mod frame;
pub mod signer;

#[cfg(feature = "iroh")]
pub use certify::{Approver, CertificationOutcome, CertifyError, request_certification};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound};
#[cfg(feature = "iroh")]
pub use environment::{
    EnvironmentClientError, LegacyWindowStream, PtyEvent, PtyStream, RemoteRelay,
    attach_remote_environment, open_legacy_windows, open_remote_environment,
};
pub use signer::{DeviceSigner, MaybeSendSync, SignError, SignFuture};
pub use tilo_relay_protocol::transport::{ContentPeerRegistration, Envelope};
