#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "integration tests assert by panicking, and the helpers they share live outside `#[test]` functions"
)]

mod abi_additions;
mod blocks;
mod common;
mod components;
mod errors;
mod ids_lock;
mod language;
mod layout;
mod multifile;
mod native;
mod origins;
mod outcomes;
mod principal;
mod roundtrip;
mod search;
mod semantics;
mod unions;
mod wasm;
mod written;
