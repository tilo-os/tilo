//! Library root for the Tilo native Personal Settings app — the device and
//! account settings hub. Workspace-owned policy stays in the shell's
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
//! - [`panes`] — one module per category content pane.
//! - [`widgets`] — small composition helpers (section, setting row, item row,
//!   caption) shared across panels.
//! - [`accounts`] — the **shared external-accounts store** Settings owns
//!   and Mail / Calendar / file apps consume.
//! - the **shared region & formats store** (global user locale) is the
//!   `tilo-locale` crate: Settings owns it and Sheets / Weather consume it.
//! - [`prefs`] — the **shared system-preferences store** (device sounds,
//!   display comfort, pointer/keyboard behaviour, update policy) Settings
//!   owns and the compositor / update service consume.
//! - [`dispatcher`] — the `os-actions` plumbing the hardware panes share.

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

pub(crate) fn take_launcher_provider_admin() -> ui::models::ProviderAdministration {
    LAUNCHER_PROVIDER_ADMIN.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

mod accessibility_settings;
pub mod account_probe;
pub mod accounts;
pub mod categories;
pub(crate) mod channel;
pub mod resource;
pub mod row;
pub mod scenarios;
pub use os_actions::codec_pack;
mod community;
mod continuity;
mod device_fleet;
pub(crate) mod device_read;
pub mod dispatcher;
pub mod effects;
pub mod host;
pub mod i18n;
pub(crate) mod input_sources;
pub mod keyboard_preview;
pub mod panes;
pub mod prefs;
pub(crate) mod radios;
pub mod search;
pub mod settings_app;
#[cfg(test)]
mod settings_write_tests;
pub mod sidebar;
pub mod speaker_setup;
pub(crate) mod watch;
pub mod widgets;

pub use settings_app::SettingsApp;

pub use resource::{Settings, SettingsShell, parse_settings_uri};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(SettingsShell)
}

#[cfg(test)]
mod cpu_profile;
#[cfg(test)]
mod reveal_tests;
#[cfg(test)]
mod scroll_tests;
#[cfg(test)]
mod ui_thread_tests;
