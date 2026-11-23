#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

#[path = "fixture.rs"]
pub mod fixture;

#[path = "build.rs"]
pub mod build;

#[path = "seal.rs"]
pub mod seal;
