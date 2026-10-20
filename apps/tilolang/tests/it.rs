//! The runtime as the framework runs it, with the Notes package installed: the real component
//! runtime, storage, permissions, history and door, without a window on a screen.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod door;
mod support;
