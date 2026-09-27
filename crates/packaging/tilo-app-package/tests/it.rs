// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "fixtures.rs"]
mod fixtures;

#[path = "app_manifests.rs"]
mod app_manifests;
#[path = "manifest.rs"]
mod manifest;
#[path = "pack_unpack.rs"]
mod pack_unpack;
#[path = "verify_attest.rs"]
mod verify_attest;
#[path = "web_build.rs"]
mod web_build;
