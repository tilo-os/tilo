#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

#[path = "support/mod.rs"]
mod support;

#[path = "install.rs"]
mod install;

#[path = "dev.rs"]
mod dev;

#[path = "image.rs"]
mod image;
