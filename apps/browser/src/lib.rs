#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_field_names,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Browser library root: the [`BrowserShell`] framework app, whose surface
//! is the tabbed [`BrowserTabs`] workspace of [`BrowserWidget`] pages.
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
mod address_controls;
pub mod address_overlay;
pub mod autofill;
mod back_hold;
pub mod blocker;
pub mod browser_tab_strip;
pub mod browser_tabs;
pub mod browser_widget;
pub mod browsing_mode;
mod credentials;
mod daily_controls;
pub mod downloads;
pub mod effects;
pub mod extensions;
pub mod history;
pub mod library;
mod native_blocker;
mod page_menu;
pub mod private_window;
pub mod resource;
pub mod search_provider;
pub mod session;
pub mod settings;
mod site_controls;
pub mod storage;

pub use browser_tab_strip::BrowserTabStrip;
pub use browser_tabs::BrowserTabs;
pub use browser_widget::BrowserWidget;

mod capabilities;
mod live_capture;

pub use resource::{Browser, BrowserShell};

ui::dev_reload_entry!(BrowserShell);
