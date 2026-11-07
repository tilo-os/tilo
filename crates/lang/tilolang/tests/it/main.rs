#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "integration tests assert by panicking, and the helpers they share live outside `#[test]` functions"
)]

mod abi_additions;
mod agent;
mod blocks;
mod common;
mod components;
mod coverage;
mod effects;
mod errors;
mod ids_lock;
mod json;
mod language;
mod layout;
mod multifile;
mod native;
mod origins;
mod outcomes;
mod placement;
mod principal;
mod repeats;
mod roundtrip;
mod search;
mod semantics;
mod text_math;
mod triggers;
mod unions;
mod wasm;
mod width;
mod written;
