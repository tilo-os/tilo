//! The panel's headless tests, one binary: `layout.rs` and `scale.rs` are its
//! modules and share one `common`. A file a binary linked the whole panel and
//! its toolkit once each, and each carried its own copy of what they share.
//!
//! The tests that start a session (`session.rs`) keep a binary of their own,
//! which links the compositor and the session these never start, and so do
//! the ones that read the panel over AT-SPI (`accessibility.rs`), which name
//! their buses in the process's environment for the panel they start.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

mod common;
mod layout;
mod scale;
