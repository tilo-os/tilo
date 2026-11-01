//! Wire contract of Tilo's browser-facing delivery.
//!
//! Every type here is an inert, validated record: the private action and
//! agent-read vocabularies; the v2 live envelope; and the workspace join contract.
//! Browsers, the action broker and both hosts link this crate without linking the
//! relay daemon. The live presence and ops frames are `tilo_relay_protocol::live`.
#![deny(rust_2018_idioms)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod peer_actions;
pub mod transport_v2;
pub mod workspace_join;
