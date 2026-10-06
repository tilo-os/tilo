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
//! - [`actions`]   — the [`actions::DbAction`] CRUD of the saved-connection list
//!   and saved queries, and the SQL buffer's edits (collaborative doc state).
//! - [`resource`]  — the workspace resource and the framework app over it.
//! - [`sample`]    — the bundled offline sample database.
//! - [`session`]   — runtime view state (connections, schema, results).
//! - [`doc`]       — the CRDT-backed SQL buffer + saved-connection list.
//! - `ui::CodeEditor` — the shared CRDT-backed SQL editing surface.
//! - [`grid`]      — the sortable, paginated, horizontally-scrollable results grid.
//! - `schema_tree` — the database → schema → table → column browser.
//! - [`structure`] — the columns/keys/indexes inspector.
//! - [`connect_modal`] — the new-connection form.
//! - [`app`]       — the root widget that composes them all.

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
pub use resource::{DbClient, DbClientShell};
