//! The platform foundation without wasm: records, replayable actions, surface, UI graph.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

mod fixtures;
mod records;
mod surface;
mod ui;
