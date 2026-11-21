#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One binary for the suites that redirect the process-global XDG config root,
// kept out of `it` so they never share a process with the UI interaction suite.
// `#[path]` keeps the files in place.

#[path = "accounts_mail_loop.rs"]
mod accounts_mail_loop;
#[path = "prefs_persistence.rs"]
mod prefs_persistence;
