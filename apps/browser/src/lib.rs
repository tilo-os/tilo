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
// The parts compiled into the tests (below) are public in browser-parts for
// Browser alone; their errors and panics are documented where Browser
// handles them.
#![cfg_attr(
    test,
    allow(
        clippy::missing_errors_doc,
        clippy::missing_panics_doc,
        clippy::new_without_default,
        reason = "public only to Browser; see browser-parts"
    )
)]

#[cfg(not(test))]
pub use browser_parts::actions;
#[cfg(not(test))]
pub use browser_parts::address_controls;
pub mod address_overlay;
#[cfg(not(test))]
pub use browser_parts::autofill;
#[cfg(not(test))]
pub use browser_parts::back_hold;
#[cfg(not(test))]
pub use browser_parts::blocker;
pub mod browser_widget;
pub mod browser_window;
#[cfg(not(test))]
pub use browser_parts::browsing_mode;
#[cfg(not(test))]
pub use browser_parts::capabilities;
pub mod continuity;
mod continuity_handoff;
mod continuity_library;
pub mod continuity_sources;
#[cfg(not(test))]
pub use browser_parts::credentials;
#[cfg(not(test))]
pub use browser_parts::crx;
mod daily_controls;
#[cfg(not(test))]
pub use browser_parts::downloads;
#[cfg(not(test))]
pub use browser_parts::effects;
#[cfg(not(test))]
pub use browser_parts::engine_setup;
#[cfg(not(test))]
pub use browser_parts::extension_controller;
#[cfg(not(test))]
pub use browser_parts::extension_host;
#[cfg(not(test))]
pub use browser_parts::extension_notices;
#[cfg(not(test))]
pub use browser_parts::extension_omnibox;
#[cfg(not(test))]
pub use browser_parts::extension_toolbar;
#[cfg(not(test))]
pub use browser_parts::extensions;
pub mod history;
pub mod imported_folders;
#[cfg(not(test))]
pub use browser_parts::library;
#[cfg(not(test))]
pub use browser_parts::native_blocker;
#[cfg(not(test))]
pub use browser_parts::notice;
pub mod omnibox;
#[cfg(not(test))]
pub use browser_parts::page_dialogs;
#[cfg(not(test))]
pub use browser_parts::page_menu;
mod personal_bookmark_view;
pub mod personal_bookmarks;
#[cfg(not(test))]
pub use browser_parts::private_window;
pub mod resource;
#[cfg(not(test))]
pub use browser_parts::rows;
pub mod search_provider;
#[cfg(not(test))]
pub use browser_parts::service_destinations;
#[cfg(not(test))]
pub use browser_parts::service_profiles;
#[cfg(not(test))]
pub(crate) use browser_parts::service_sessions;
pub mod settings;
#[cfg(not(test))]
pub use browser_parts::page_popups;
#[cfg(not(test))]
pub use browser_parts::site_controls;
#[cfg(not(test))]
pub use browser_parts::storage;
#[cfg(not(test))]
pub use browser_parts::web_store;
pub use service_sessions::service_session_capability;

// Browser's copy is `browser-parts`'s catalogue, which both resolve.
pub(crate) use browser_parts::CATALOG;

// The parts compile into Browser's own tests, which test them with the
// browser they serve; outside the tests they are `browser-parts`'s.
#[cfg(test)]
#[path = "../../browser-parts/src"]
mod parts {
    pub mod actions;
    pub mod address_controls;
    pub mod autofill;
    pub mod back_hold;
    pub mod blocker;
    pub mod browsing_mode;
    pub mod capabilities;
    pub mod credentials;
    pub mod crx;
    pub mod downloads;
    pub mod effects;
    pub mod engine_setup;
    pub mod extension_controller;
    pub mod extension_host;
    pub mod extension_notices;
    pub mod extension_omnibox;
    pub mod extension_toolbar;
    pub mod extensions;
    pub mod library;
    pub mod native_blocker;
    pub mod notice;
    pub mod page_dialogs;
    pub mod page_menu;
    pub mod page_popups;
    pub mod private_window;
    pub mod rows;
    pub mod service_destinations;
    pub mod service_profiles;
    pub mod service_sessions;
    pub mod site_controls;
    pub mod storage;
    pub mod web_store;
}
#[cfg(test)]
pub(crate) use parts::service_sessions;
#[cfg(test)]
pub use parts::{
    actions, address_controls, autofill, back_hold, blocker, browsing_mode, capabilities,
    credentials, crx, downloads, effects, engine_setup, extension_controller, extension_host,
    extension_notices, extension_omnibox, extension_toolbar, extensions, library, native_blocker,
    notice, page_dialogs, page_menu, page_popups, private_window, rows, service_destinations,
    service_profiles, site_controls, storage, web_store,
};

pub use browser_widget::BrowserWidget;
pub use browser_window::BrowserWindow;

mod live_capture;

pub use resource::{Browser, BrowserShell};

ui::dev_reload_entry!(BrowserShell);
