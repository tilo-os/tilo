//! One test binary. `tests/host.rs` and `tests/workspace_mail.rs` each linked the whole host as a
//! binary of its own; they are modules of this one. A test still runs in a process of its own
//! under nextest, so what one file's tests hold in a static is never another's.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

mod process;
mod workspace_mail;
