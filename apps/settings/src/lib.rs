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
//! - [`region`] — the **shared region & formats store** (global user
//!   locale) Settings owns and Sheets / Weather consume.
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
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod account_probe;
pub mod accounts;
pub mod categories;
pub mod resource;
pub mod scenarios;
pub use os_actions::codec_pack;
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
pub mod region;
pub mod search;
pub mod settings_app;
pub mod sidebar;
pub mod speaker_setup;
pub mod widgets;

pub use settings_app::SettingsApp;

pub use resource::{Settings, SettingsShell, parse_settings_uri};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<SettingsShell>()
}

#[cfg(test)]
mod cpu_profile;
