//! Database — a Tilo native database client built on the `ui` toolkit.
//!
//! Connects to `PostgreSQL`, `MySQL` and `SQLite` over real drivers. `SQLite` runs
//! fully locally (and ships a bundled sample catalog for the offline `--shots`
//! path); `PostgreSQL` and `MySQL` open real TCP connections, introspect the live
//! catalog and run real queries — a refused socket or a bad password surfaces
//! as an honest error, never a fabricated success.
//!
//! Layers:
//! - [`model`]        — connection profiles, schema types, query results.
//! - [`engine`]       — the `SQLite` driver, the [`engine::LiveEngine`] enum that
//!   unifies all three engines, and [`engine::open_engine`].
//! - [`engine_pg`]    — the `PostgreSQL` driver: introspection + query execution.
//! - [`engine_mysql`] — the MySQL/MariaDB driver: introspection + execution.
//! - [`worker`]    — the background thread that owns the live engine and runs
//!   every blocking connect/introspect/query off the UI thread.
//! - [`actions`]   — the [`actions::DbAction`] CRUD of the saved-connection list,
//!   routed through [`ui::ActionHost`] (collaborative doc state).
//! - [`sample`]    — the bundled offline sample database.
//! - [`session`]   — runtime view state (connections, schema, results).
//! - [`doc`]       — the CRDT-backed SQL buffer + saved-connection list.
//! - `ui::CodeEditor` — the shared CRDT-backed SQL editing surface.
//! - [`grid`]      — the sortable, paginated, horizontally-scrollable results grid.
//! - `schema_tree` — the database → schema → table → column browser.
//! - [`structure`] — the columns/keys/indexes inspector.
//! - [`connect_modal`] — the new-connection form.
//! - [`app`]       — the root widget that composes them all.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
pub mod app;
pub mod connect_modal;
pub mod doc;
pub mod engine;
pub mod engine_mysql;
pub mod engine_pg;
pub mod grid;
pub mod model;
pub mod resource;
pub mod sample;
pub mod schema_nav;
pub mod search;
pub mod session;
pub mod structure;
pub mod theme_ext;
pub mod worker;

pub use app::DbClientApp;

use ui::widget::Widget;

/// Build the live app: it opens on the user's saved connections (empty on a
/// fresh machine) and never fabricates a sample database or profiles.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(DbClientApp::new())
}

/// Build a fully-populated app for the hermetic `--shots`/demo path — the
/// sole source of the bundled sample database and connection profiles.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(DbClientApp::new_demo())
}

/// Build the demo root for an offscreen `--shots` frame at a given width,
/// with the responsive sidebar flap pre-snapped to that width's layout
/// default (wide ⇒ docked+open, narrow ⇒ overlay+collapsed). This makes the
/// single offscreen frame deterministic without waiting for the eased slide.
pub fn app_root_for_shot(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new_demo();
    app.snap_flap_for_width(width);
    Box::new(app)
}

/// Like [`app_root_for_shot`] but the real live (empty) app, so the first-run
/// empty state can be screenshotted.
pub fn app_root_for_shot_empty(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new();
    app.snap_flap_for_width(width);
    Box::new(app)
}

/// Like [`app_root_for_shot`] but with the floating status bubble armed fully
/// visible, so the chip that replaced the old bottom status strip can be
/// screenshotted at the bottom-right of the populated demo.
pub fn app_root_for_shot_status(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new_demo();
    app.snap_flap_for_width(width);
    app.arm_status_bubble_for_shots();
    Box::new(app)
}

/// Like [`app_root_for_shot`] with the "New connection" sheet open.
pub fn app_root_for_shot_connect(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new_demo();
    app.snap_flap_for_width(width);
    app.open_connect_modal_for_shots();
    Box::new(app)
}

/// Like [`app_root_for_shot`] with the destructive-statement confirmation
/// dialog armed.
pub fn app_root_for_shot_confirm(width: f64) -> Box<dyn Widget> {
    let mut app = DbClientApp::new_demo();
    app.snap_flap_for_width(width);
    app.arm_query_confirmation_for_shots();
    Box::new(app)
}
#[cfg(target_arch = "wasm32")]
pub use resource::{DbClient, DbClientShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{DbClient, DbClientShell, launch_spec};
