// One test binary: files stay separate, #[path] pulls them in as modules.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "fixture.rs"]
pub mod fixture;

#[path = "build.rs"]
pub mod build;

#[path = "seal.rs"]
pub mod seal;
