//! The desktop surface's integration tests, as one binary.
//!
//! Each file under `tests/` was a binary of its own that linked this crate, the toolkit, the
//! chrome over the windows and the media stack again. They are modules of this one instead, each
//! where it was. A test still runs in a process of its own under nextest, and under `cargo test`
//! on a thread of its own, which is all any of them shares state by: the locale a test reads in,
//! the clock a gesture test steps and the launches a shell records are thread-locals.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

mod support;

mod keyboard;
mod peer_page;
mod peer_view;
mod retained_owner;
mod surface;
