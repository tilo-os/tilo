#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

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
#[path = "support.rs"]
mod support;
#[path = "trust.rs"]
mod trust;
