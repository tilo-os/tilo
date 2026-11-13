//! Backups — snapshots of a folder to a destination, verified, with a retention policy.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
pub mod engine;
mod i18n;
pub mod model;
pub mod resource;
pub mod search;
mod view;

pub use resource::{BackupLocation, Backups, BackupsShell};
