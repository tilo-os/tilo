//! Quick Browser library root. The binary is a thin shim over the
//! `BrowserWidget` exported here so the `--shots` mode can construct
//! the whole chrome without booting an event loop.
//!
//! The browser is deliberately small: it is chrome (address bar,
//! navigation, progress, filter loading) around the `ui` toolkit's
//! `WebView` widget. The web engine itself lives behind
//! `ui::webview::WebViewEngine`; there is no tab UI — tabbing is the
//! compositor's `WindowGroup` frame primitive.

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
pub mod address_overlay;
pub mod blocker;
pub mod browser_widget;
/// Zero-copy WebView compositor — needs the WPE engine's dmabuf import.
#[cfg(feature = "wpe")]
pub mod compositor;
pub mod downloads;
pub mod extensions;
pub mod history;
pub mod library;
pub mod search_provider;
pub mod settings;

pub use browser_widget::BrowserWidget;

/// Production Browser root used by the normal launcher, headless tooling,
/// and certification. Launch-specific URI handling remains in the binary;
/// this deliberately creates the same safe default profile a user gets when
/// opening Browser without a target.
///
/// # Errors
///
/// Returns an error when the production WPE engine is absent or cannot start.
/// Callers must present that failure instead of substituting the deterministic
/// test engine used by headless tests.
pub fn app_root() -> Result<BrowserWidget, String> {
    BrowserWidget::try_new()
}

#[cfg(all(test, not(feature = "wpe")))]
mod tests {
    use super::app_root;

    #[test]
    fn production_root_refuses_builds_without_wpe() {
        let Err(error) = app_root() else {
            panic!("live Browser must not use the test engine");
        };
        assert!(error.contains("built without the WPE engine"), "{error}");
    }
}
