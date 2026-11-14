//! The desktop surface's integration tests, as one binary.
//!
//! Each file under `tests/` was a binary of its own that linked this crate, the toolkit, the
//! chrome over the windows and the media stack again. They are modules of this one instead, each
//! where it was. A test still runs in a process of its own under nextest, and under `cargo test`
//! on a thread of its own, which is all any of them shares state by: the locale a test reads in,
//! the clock a gesture test steps and the launches a shell records are thread-locals.

mod support;

mod every_web_app_opens_from_search;
mod keyboard;
mod peer_page;
mod peer_view;
mod retained_owner;
mod surface;
