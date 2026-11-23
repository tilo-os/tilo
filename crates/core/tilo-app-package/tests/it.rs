#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

#[path = "fixtures.rs"]
mod fixtures;

#[path = "app_manifests.rs"]
mod app_manifests;
#[path = "format_1_app.rs"]
mod format_1_app;
#[path = "image.rs"]
mod image;
#[path = "manifest.rs"]
mod manifest;
#[path = "pack_unpack.rs"]
mod pack_unpack;
#[path = "verify_attest.rs"]
mod verify_attest;
