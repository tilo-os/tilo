//! Serialized, no-I/O exchange state for Connected protocol frames.
//!
//! This core neither authenticates a peer nor interprets a response as
//! approval, success, durability, permission, or replay authority. A future
//! driver must own transport deadlines, cancellation, and physical closure.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::panic_in_result_fn,
        clippy::unreachable,
        clippy::arithmetic_side_effects,
        reason = "in a test these operations form assertions over bounded fixtures"
    )
)]

mod exchange;

pub use exchange::{
    BeginError, BeginFailure, ClientCore, ExchangeError, FeedResponse, OutboundRequest,
    TerminalOutcome, UnvalidatedResponse,
};
