// One test binary instead of one per file; #[path] keeps the files in place.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "fake_broker.rs"]
mod fake_broker;
#[path = "mock_broker.rs"]
mod mock_broker;
#[path = "schema_v2.rs"]
mod schema_v2;
#[path = "schema_v3.rs"]
mod schema_v3;
