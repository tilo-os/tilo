// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/mod.rs"]
mod support;

#[path = "install.rs"]
mod install;

#[path = "image.rs"]
mod image;
