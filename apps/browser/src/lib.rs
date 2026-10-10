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

pub mod actions;
mod address_controls;
pub mod address_overlay;
pub mod autofill;
mod back_hold;
pub mod blocker;
pub mod browser_widget;
pub mod browser_window;
pub mod browsing_mode;
mod credentials;
mod daily_controls;
pub mod downloads;
pub mod effects;
pub mod extensions;
pub mod history;
pub mod library;
mod native_blocker;
mod notice;
mod page_menu;
pub mod private_window;
pub mod resource;
mod rows;
pub mod search_provider;
pub mod settings;
mod site_controls;
pub mod storage;

pub use browser_widget::BrowserWidget;
pub use browser_window::BrowserWindow;

mod capabilities;
mod live_capture;

pub use resource::{Browser, BrowserShell};

ui::dev_reload_entry!(BrowserShell);
