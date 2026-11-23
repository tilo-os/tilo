#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

mod abi_additions;
mod actor;
mod agent;
mod blocks;
mod calendar_origins;
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
mod provenance;
mod roundtrip;
mod search;
mod semantics;
mod shadowing;
mod text_math;
mod titles;
mod triggers;
mod unions;
mod wasm;
mod width;
mod written;
