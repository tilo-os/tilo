//! The menu of the window in front as a view of the session's
//! `desktop:window-menu` resource and nothing else.
//!
//! The session publishes what the window offers — the rows, whether each can
//! be chosen and why one cannot, the window they are for and where the menu
//! stands — and the three verbs that open it, close it and choose a row.
//! This app draws that reading as a context menu and invokes those verbs; it
//! keeps no menu state of its own, so a menu it draws and a menu a script
//! drives are the same menu, and closing it anywhere closes it here.

pub mod resource;
pub mod surface;

pub use resource::{WindowMenu, WindowMenuApp};
pub use surface::WindowMenuSurface;

/// The view's package id.
pub const APP_ID: &str = "so.tilo.window-menu";
