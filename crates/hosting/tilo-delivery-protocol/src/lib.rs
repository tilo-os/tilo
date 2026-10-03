//! Wire contract of Tilo's browser-facing delivery.
//!
//! Every type here is an inert, validated record: delivery v3 commits, cursors,
//! descriptors, authentication and HTTP proofs, the `LIVE_V1`, media, onboarding and
//! private wires; the private action and agent-read vocabularies; the v2 live
//! envelope; and the workspace join contract. Browsers, the action broker and
//! both hosts link this crate without linking the relay daemon.
#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod clock;
pub mod delivery_v3;
pub mod peer_actions;
pub mod transport_v2;
pub mod workspace_join;

#[cfg(all(test, not(target_arch = "wasm32")))]
tilo_automerge_isolate::test_worker!();
