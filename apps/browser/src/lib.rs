#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_field_names,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Browser library root: the [`BrowserShell`] framework app, whose surface
//! is a [`BrowserWindow`] showing one [`BrowserWidget`] page. Other pages are
//! other Browser instances in the desktop's tabs.
//!
//! The browser is deliberately small: it is chrome (address bar,
//! navigation, progress, filter loading) around the `ui` toolkit's
//! `WebView` widget. The web engine itself lives behind
//! `ui::webview::WebViewEngine`; retained tabs share one native runtime.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
// Their items are public for Browser's own modules and tests, not as an
// API: errors and panics are documented where Browser handles them.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::new_without_default,
    reason = "public for Browser's own modules, not an API"
)]

pub mod actions;
pub mod address_controls;
pub mod address_overlay;
pub mod autofill;
pub mod back_hold;
pub mod blocker;
pub mod browser_widget;
pub mod browser_window;
pub mod browsing_mode;
pub mod capabilities;
pub mod continuity;
mod continuity_handoff;
mod continuity_library;
pub mod continuity_sources;
pub mod credentials;
pub mod crx;
mod daily_controls;
pub mod downloads;
pub mod effects;
pub mod engine_setup;
pub mod extension_controller;
pub mod extension_host;
pub mod extension_notices;
pub mod extension_omnibox;
pub mod extension_toolbar;
pub mod extensions;
pub mod history;
pub mod imported_folders;
pub mod library;
pub mod native_blocker;
pub mod notice;
pub mod omnibox;
pub mod page_dialogs;
pub mod page_menu;
pub mod page_popups;
mod personal_bookmark_view;
pub mod personal_bookmarks;
pub mod private_window;
pub mod resource;
pub mod rows;
pub mod search_provider;
pub mod service_destinations;
pub mod service_profiles;
pub(crate) mod service_sessions;
pub mod settings;
pub mod site_controls;
pub mod storage;
pub mod web_store;
pub use service_sessions::service_session_capability;

// Browser's copy.
ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

pub use browser_widget::BrowserWidget;
pub use browser_window::BrowserWindow;

mod live_capture;

pub use resource::{Browser, BrowserShell};

ui::dev_reload_entry!(BrowserShell);

#[cfg(test)]
mod action_keywords_tests;
