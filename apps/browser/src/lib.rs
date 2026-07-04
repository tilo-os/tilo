//! Quick Browser library root. The binary is a thin shim over the
//! `BrowserWidget` exported here. Phase B of the iced→quick-ui
//! migration moved every UI module out of `main.rs` so the `--shots`
//! mode (Phase C) can construct a `BrowserWidget` without booting an
//! event loop.

pub mod blocker;
pub mod browser_widget;
pub mod engine;
pub mod host;
pub mod servo_engine;
pub mod servo_surface;

pub use browser_widget::BrowserWidget;
