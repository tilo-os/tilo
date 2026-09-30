//! The native desktop runner of the Tilo app SDK.
//!
//! `tilo-app` depends on this crate only on native targets, so the native
//! runner (winit, AccessKit, the Wayland/X11 clipboard, tokio's full runtime,
//! the local session daemons) never reaches a web build. Apps do not depend
//! on it; they call `ui::launch::run`, which resolves here on Linux and macOS.

pub use ui_core::launch::{run, run_or_exit};
