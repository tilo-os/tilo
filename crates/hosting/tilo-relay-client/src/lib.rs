//! `tilo-relay-client` — the device side of the Tilo host + relay.
//!
//! - [`client`] — [`RelayClient`], an authenticated session with a relay,
//!   plus the one-shot content-discovery and home-peer management calls;
//! - [`coedit`] — [`RelayCoeditSession`], the toolkit co-edit verbs mapped
//!   onto relay envelopes;
//! - [`frame`] — the length-prefixed frame codec every relay stream uses.
//!   The daemon reads and writes its end through the same functions.
//!
//! The messages themselves are defined in [`tilo_relay_protocol`].

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

pub mod client;
pub mod coedit;
pub mod frame;

pub use client::{ClientError, RelayClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound, RelayCoeditSession};
pub use tilo_relay_protocol::transport::{ContentPeerRegistration, Envelope};
