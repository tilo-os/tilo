// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "eviction.rs"]
mod eviction;
#[path = "image_apps.rs"]
mod image_apps;
#[path = "language_pack.rs"]
mod language_pack;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "runtime_cli.rs"]
mod runtime_cli;
#[path = "runtime_package.rs"]
mod runtime_package;
#[path = "trust.rs"]
mod trust;
