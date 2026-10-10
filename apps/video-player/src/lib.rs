//! A local video player for Tilo, built on the shared [`media`] streaming layer
//! and the reusable [`ui::widget::VideoPlayer`] widget.
//!
//! The binary is a thin entry point ([`crate`]'s `main`); all logic lives here.
//! [`PlayerApp`] is the surface [`VideoPlayerShell`] mounts over the shared
//! library resource.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
mod i18n;
mod kept;
mod preferences;
pub mod resource;
pub mod shared;
pub use app::{PlayerApp, source_for_argument, source_for_path};
pub use resource::{VideoPlayer, VideoPlayerShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<VideoPlayerShell>()
}
