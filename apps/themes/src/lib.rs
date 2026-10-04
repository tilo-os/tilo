//! Themes — Tilo native theme studio (CRDT-native, built on the `ui`
//! toolkit). Browse preset themes, customise the OS token set live, and
//! create / duplicate / export your own.
//!
//! [`ThemesShell`] is the app: every host mounts it through [`definition`],
//! and the binary runs it with every standard launch mode, `--shots`
//! included.

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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod customizer;
pub mod doc;
pub mod draw;
pub mod gallery;
pub mod model;
pub mod preview;
pub mod resource;
pub mod store;
pub mod widgets;

pub use app::ThemesApp;
pub use resource::{Themes, ThemesEffect, ThemesShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<ThemesShell>()
}
