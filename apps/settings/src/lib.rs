//! Library root for the qOS native Settings app — the coherent OS
//! settings hub.
//!
//! `SettingsApp` (in [`settings_app`]) is the root widget: a categorized
//! sidebar plus a content pane per category, responsive between a two-pane
//! desktop layout and a single-pane narrow layout. The binary in `main.rs`
//! is a thin shim that hands the widget to `ui::App` for the live event
//! loop, or renders one offscreen frame for `--shots`.
//!
//! ## Module map
//!
//! - [`categories`] — the sidebar's [`categories::Section`]s and
//!   [`categories::Category`]s.
//! - [`panes`] — one module per category content pane.
//! - [`widgets`] — small composition helpers (card, setting row, caption)
//!   shared across panels.
//! - [`accounts`] — the **shared external-accounts store** Settings owns
//!   and Mail / Calendar / file apps consume.
//! - [`region`] — the **shared region & formats store** (global user
//!   locale) Settings owns and Sheets / Weather consume.
//! - [`dispatcher`] — the `os-actions` plumbing the hardware panes share.

pub mod accounts;
pub mod categories;
pub mod dispatcher;
pub mod panes;
pub mod region;
pub mod settings_app;
pub mod shots;
pub mod widgets;

pub use settings_app::SettingsApp;
pub use shots::render_to_path;
