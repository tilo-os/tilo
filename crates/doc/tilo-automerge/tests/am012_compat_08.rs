//! Runs `vendor/automerge/tests/resource_limits_compat_08.rs` inside the workspace: the vendored
//! automerge crate is excluded from the workspace, so its own integration tests cannot be
//! built with plain `cargo test` from the root. The suite is written against the public
//! `automerge::resource_limits` API only.
#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "vendored upstream-style test suite, linted with the vendored crate's rules"
)]

#[path = "../../../../vendor/automerge/tests/resource_limits_compat_08.rs"]
mod suite;
