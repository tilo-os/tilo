//! The Tilo native Personal Settings app: the window and its core
//! (`settings-core`), the panes people open often (here) and the panes
//! people rarely open (`settings-rare`, loaded from the Settings engine
//! library). Everything `settings-core` names is re-exported here, so the
//! app reads as one crate; [`SettingsShell`] brings the parts together
//! before the window opens.

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
// Settings' panes are tested in settings-core's own tests, which compile
// them in with the test helpers of the window they need; this crate's own
// test build leaves them out, and its integration suites test the app.
// The modules of the panes people rarely open, named here as Settings' own.
#[cfg(not(test))]
pub use settings_rare::{account_probe, device_fleet, speaker_setup};

#[cfg(not(test))]
pub mod dispatcher;
#[cfg(not(test))]
pub(crate) mod input_sources;
#[cfg(not(test))]
pub mod keyboard_preview;
#[cfg(not(test))]
pub mod panes;

#[cfg(not(test))]
mod shell;
#[cfg(not(test))]
pub use shell::SettingsShell;

/// The resource Settings presents, and the app over it.
#[cfg(not(test))]
pub mod resource {
    pub use crate::shell::SettingsShell;
    pub use settings_core::resource::*;
}

/// Bring the parts of Settings together: its own, the panes people open
/// often and the panes people rarely open. Every entry point does this
/// before the window opens; calling it again keeps the first.
#[cfg(not(test))]
pub fn install() {
    settings_core::area::install(panes::PARTS);
}

/// Capture the launcher's explicit administration grant, independently of model selection.
#[cfg(not(test))]
pub fn enable_provider_administration() {
    install();
    settings_core::enable_provider_administration();
}

/// The app as every host mounts it.
#[cfg(not(test))]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    install();
    ui::built_in_definition!(SettingsShell)
}
