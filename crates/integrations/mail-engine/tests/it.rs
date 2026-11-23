#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file: each tests/*.rs linked the whole engine
// separately. #[path] keeps the files where they are, and nextest runs each
// test in its own process, so the isolation separate binaries gave them stays.
// The files that drive the live engine keep their own `live` check.

#[path = "account_engine_loop.rs"]
mod account_engine_loop;
#[path = "recipient_headers.rs"]
mod recipient_headers;
