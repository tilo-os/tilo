//! Quick Browser library root. The binary is a thin shim over the
//! `BrowserWidget` exported here so the `--shots` mode can construct
//! the whole chrome without booting an event loop.
//!
//! The browser is deliberately small: it is chrome (address bar,
//! navigation, progress, filter loading) around the `ui` toolkit's
//! `WebView` widget. The web engine itself lives behind
//! `ui::webview::WebViewEngine`; there is no tab UI — tabbing is the
//! compositor's `WindowGroup` frame primitive.

pub mod address_overlay;
pub mod blocker;
pub mod browser_widget;
/// Zero-copy WebView compositor — needs the WPE engine's dmabuf import.
#[cfg(feature = "wpe")]
pub mod compositor;
pub mod extensions;
pub mod history;
pub mod settings;

pub use browser_widget::BrowserWidget;
