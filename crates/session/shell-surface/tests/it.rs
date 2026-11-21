//! The surface host's integration tests, one binary instead of one per file: each `tests/*.rs`
//! linked this whole crate, and the compositor with it, as a binary of its own. Each file is a
//! module here, and nextest runs each test in a process of its own, so a test still starts from
//! the process-wide state a binary of its own gave it (the send-to box's queue, the motion
//! clock).

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

mod desktop;
mod input_frames;
mod lifted;
mod send_box_targets;
mod well;
