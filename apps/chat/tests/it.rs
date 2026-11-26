//! One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

#[path = "record_search.rs"]
mod record_search;

#[path = "native_chat_fixture.rs"]
mod native_chat_fixture;

mod assistant_grant;
