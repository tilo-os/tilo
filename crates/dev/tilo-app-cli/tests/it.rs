#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
mod android_platform;
mod json_output;
#[path = "outside_repo.rs"]
mod outside_repo;
mod package_platform;
