// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
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
