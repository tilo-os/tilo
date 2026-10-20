//! The panes of Settings people rarely open, as a library of their own:
//! the Settings engine library carries it beside Settings' core, so the
//! app's package carries only the panes people open often. Everything
//! `settings-core` names is re-exported here, so the panes read as panes of
//! Settings; they register through [`panes::PART`].

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]

pub use settings_core::*;

// The panes are tested in settings-core's own tests, which compile them in
// with the test helpers of the window they need; this crate's own test build
// leaves them out.
#[cfg(not(test))]
pub mod account_probe;
#[cfg(not(test))]
pub mod device_fleet;
#[cfg(not(test))]
pub mod panes;
#[cfg(not(test))]
pub mod speaker_setup;
