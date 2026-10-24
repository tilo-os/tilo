//! The core of the Tilo native Personal Settings app — the device and
//! account settings hub: the window, its effect vocabulary, its resource and
//! everything the panes share. The panes themselves are in the `settings`
//! app (the ones people open often) and in `settings-rare` (the ones they
//! rarely open, loaded from the Settings engine library); they register
//! with [`area::install`]. Workspace-owned policy stays in the shell's
//! contextual Workspace Settings surface; cross-cutting domains share a
//! resolver instead of letting either UI write the other's state.
//!
//! `SettingsApp` (in [`settings_app`]) is the root widget: a categorized
//! sidebar plus a content pane per category, responsive between a two-pane
//! desktop layout and a single-pane narrow layout. The binary in `main.rs`
//! runs [`SettingsShell`] on the app framework: the live window, or one of
//! the screenshot [`scenarios`].
//! Release builds keep this composition app-local while service engines live
//! in the shared runtime, preserving independently updatable settings UI.
//!
//! ## Module map
//!
//! - [`categories`] — the sidebar's [`categories::Section`]s and
//!   [`categories::Category`]s.
//! - [`sidebar`] — the sidebar's navigation list of categories.
//! - [`widgets`] — small composition helpers (section, setting row, item row,
//!   caption) shared across panels.
//! - [`accounts`] — the **shared external-accounts store** Settings owns
//!   and Mail / Calendar / file apps consume.
//! - the **shared region & formats store** (global user locale) is the
//!   `tilo-locale` crate: Settings owns it and Sheets / Weather consume it.
//! - [`prefs`] — the **shared system-preferences store** (device sounds,
//!   display comfort, pointer/keyboard behaviour, update policy) Settings
//!   owns and the compositor / update service consume.

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
// What this crate makes public it makes public for the Settings app's own
// two other crates (the panes people open often and those they rarely
// open), not as an API: the errors and panics of its items are documented
// where they are handled, as they were when the three were one crate.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::new_without_default,
    reason = "public only to the Settings app's own crates; see the note above"
)]
thread_local! {
    static LAUNCHER_PROVIDER_ADMIN: std::cell::RefCell<ui::models::ProviderAdministration> =
        std::cell::RefCell::default();
}

/// Capture the launcher's explicit administration grant, independently of model selection.
pub fn enable_provider_administration() {
    LAUNCHER_PROVIDER_ADMIN.with(|slot| {
        *slot.borrow_mut() =
            ui::models::ProviderAdministration::from_launcher().unwrap_or_default();
    });
}

pub fn take_launcher_provider_admin() -> ui::models::ProviderAdministration {
    LAUNCHER_PROVIDER_ADMIN.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

pub use settings_sections::accessibility as accessibility_settings;
pub mod accounts;
pub mod area;
pub mod categories;
pub mod channel;
pub mod resource;
pub mod row;
pub mod scenarios;
pub use os_actions::codec_pack;
pub mod continuity;
pub mod device_read;
pub mod effects;
pub mod host;
pub mod i18n;
pub mod pages;
pub mod pane_model;
pub mod prefs;
pub mod radios;
pub mod search;
pub mod secrets;
pub mod setting;
pub mod settings_app;
#[cfg(test)]
mod settings_write_tests;
pub mod sidebar;
#[cfg(test)]
pub(crate) mod stall;
pub mod watch;
pub mod widgets;

// The panes compile into this crate's own tests, so the window is tested
// with every pane, as it runs; outside the tests they are the `settings`
// app's and `settings-rare`'s.
#[cfg(test)]
#[path = "../../settings-rare/src/account_probe.rs"]
pub mod account_probe;
#[cfg(test)]
#[path = "../../settings-rare/src/device_fleet.rs"]
pub mod device_fleet;
#[cfg(test)]
#[path = "../../settings/src/dispatcher.rs"]
pub mod dispatcher;
#[cfg(test)]
#[path = "../../settings/src/input_sources.rs"]
pub mod input_sources;
#[cfg(test)]
#[path = "../../settings/src/keyboard_preview.rs"]
pub mod keyboard_preview;
#[cfg(test)]
#[path = "test_panes.rs"]
pub mod panes;
#[cfg(test)]
#[path = "../../settings-rare/src/speaker_setup.rs"]
pub mod speaker_setup;

pub use settings_app::SettingsApp;

pub use resource::{Settings, SettingsShell, parse_settings_uri};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(SettingsShell)
}

#[cfg(test)]
mod cpu_profile;
#[cfg(test)]
mod relayout_tests;
#[cfg(test)]
mod reveal_tests;
#[cfg(test)]
mod scroll_tests;
#[cfg(test)]
mod ui_thread_tests;
