#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "integration tests assert by panicking, and the helpers they share live outside `#[test]` functions"
)]

mod blocks;
mod common;
mod errors;
mod ids_lock;
mod layout;
mod roundtrip;
mod semantics;
mod wasm;
