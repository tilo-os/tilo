//! A small, responsive internet speed test.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic over small bounded quantities"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
mod measure;
mod model;
pub mod resource;
mod view;

pub use resource::{SpeedTest, SpeedTestShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<SpeedTestShell>()
}
