//! `tilo-relay-client` — the length-prefixed frame codec every relay stream
//! uses ([`frame`]). The daemon and the relay session client in `tilo-relay`
//! read and write through the same functions.
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

pub mod frame;

pub use tilo_relay_protocol::transport::Envelope;
