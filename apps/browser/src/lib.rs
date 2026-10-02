#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_field_names,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Quick Browser library root. The binary is a thin shim over the
//! `BrowserWidget` exported here so the `--shots` mode can construct
//! the whole chrome without booting an event loop.
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
/// Zero-copy WebView compositor — needs the WPE engine's dmabuf import.
#[cfg(feature = "wpe")]
pub mod compositor;
mod credentials;
mod daily_controls;
pub mod downloads;
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
/// Production Browser root used by the normal launcher, headless tooling,
/// and certification. Launch-specific URI handling remains in the binary;
/// this deliberately creates the same safe default profile a user gets when
/// opening Browser without a target.
#[must_use]
pub fn app_root() -> BrowserWidget {
    BrowserWidget::new()
}
pub use resource::{Browser, BrowserShell, launch_spec as framework_launch_spec};
