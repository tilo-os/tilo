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
//! - [`record`]    — the shared workspace: saved connections and queries and
//!   the SQL buffer, a typed model.
//! - [`resource`]  — the workspace's verbs, effects and the framework app.
//! - [`sample`]    — the bundled offline sample database.
//! - [`session`]   — runtime view state (connections, schema, results).
//! - [`link`]      — the model the controller publishes and the intents the
//!   views answer with.
//! - [`navigator`] — the sidebar: connections and tables.
//! - [`workspace`] — the content: tabs, the views they show, and the dialogs.
//! - [`query`], [`results`], [`structure`] — the Query, Data and Structure
//!   views over the toolkit's editor and table.
//! - [`connect_modal`] — the new-connection form and the confirmation dialog.
//! - [`app`]       — the controller that owns the session and the window.

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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod connect_modal;
pub mod engine;
pub mod engine_mysql;
pub mod engine_pg;
pub mod i18n;
pub mod link;
pub mod model;
pub mod navigator;
pub mod query;
pub mod record;
pub mod resource;
pub mod results;
pub mod sample;
pub mod search;
pub mod session;
pub mod structure;
pub mod worker;
pub mod workspace;

pub use app::DbClientApp;
pub use resource::{DbClientShell, Workspace};
