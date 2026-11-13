//! A package whose field changed from `Text` to `RichText`: the records keep their words.
//!
//! A binary of its own: the packages register their schemas in a table shared by the whole
//! process, and this one loads Notes with a note's body kept as rich text, so it must not share
//! a process with the tests loading Notes as the shipped app has it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code,
    reason = "test assertions; fixtures.rs serves every test binary"
)]

#[path = "fixtures.rs"]
mod fixtures;
mod upgrade_fields;
mod window;
