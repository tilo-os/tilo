//! Runs the vendored automerge bounded-read tests (vendor/automerge is outside
//! the workspace, so its own tests cannot run with `cargo test -p automerge`).
#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "vendored upstream-style test suite, linted with the vendored crate's rules"
)]

#[path = "../../../../vendor/automerge/tests/resource_limits_read.rs"]
mod resource_limits_read;
