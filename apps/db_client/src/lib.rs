//! Database — a qOS native database client built on the `ui` toolkit.
//!
//! Connects to PostgreSQL, MySQL and SQLite. SQLite runs fully locally and
//! ships with a bundled sample catalog so the schema browser, SQL editor and
//! results grid all work offline; the network engines are described and
//! manageable but their live drivers are gated behind a sandbox grant.
//!
//! Layers:
//! - [`model`]     — connection profiles, schema types, query results.
//! - [`engine`]    — the SQLite driver: introspection + query execution.
//! - [`sample`]    — the bundled offline sample database.
//! - [`session`]   — runtime view state (connections, schema, results).
//! - [`doc`]       — the CRDT-backed SQL editor buffer.
//! - [`editor`]    — the syntax-highlighting SQL editor widget.
//! - [`grid`]      — the sortable, paginated results grid.
//! - [`schema_tree`] — the database → schema → table → column browser.
//! - [`structure`] — the columns/keys/indexes inspector.
//! - [`connect_modal`] — the new-connection form.
//! - [`app`]       — the root widget that composes them all.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod connect_modal;
pub mod doc;
pub mod editor;
pub mod engine;
pub mod grid;
pub mod model;
pub mod sample;
pub mod schema_tree;
pub mod session;
pub mod structure;
pub mod syntax;
pub mod theme_ext;

pub use app::DbClientApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(DbClientApp::new())
}

/// Build the root widget for an offscreen `--shots` frame at a given width,
/// with the responsive sidebar flap pre-snapped to that width's layout
/// default (wide ⇒ docked+open, narrow ⇒ overlay+collapsed). This makes the
/// single offscreen frame deterministic without waiting for the eased slide.
pub fn app_root_for_shot(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new();
    app.snap_flap_for_width(width);
    Box::new(app)
}
