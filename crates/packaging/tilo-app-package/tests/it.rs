// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "fixtures.rs"]
mod fixtures;

#[path = "app_manifests.rs"]
mod app_manifests;
#[path = "image.rs"]
mod image;
#[path = "manifest.rs"]
mod manifest;
#[path = "pack_unpack.rs"]
mod pack_unpack;
#[path = "verify_attest.rs"]
mod verify_attest;
