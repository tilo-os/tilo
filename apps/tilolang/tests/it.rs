//! The runtime as the framework runs it, with the Notes package installed: the real component
//! runtime, storage, permissions, history and door, without a window on a screen.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

#[allow(
    dead_code,
    reason = "each test binary reads what it needs of the change marks"
)]
mod change_marks;
mod door;
mod marks;
mod narrow;
mod opening;
mod rooms;
mod support;
mod sync;
