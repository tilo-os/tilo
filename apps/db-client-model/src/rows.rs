//! The types the catalogue's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource, and it offers the verbs its type declares.
//!
//! A catalogue row's address is the catalogue's: the kind, then each name it
//! holds hex-encoded and joined by `/` (`relation/<connection>/<schema>/<table>`),
//! so a row's address outlives the names' punctuation; what its effects act on
//! the window binds as it publishes the row.

use crate::resource::DbEffect;

/// One connected database: the connection, opened at its database.
#[derive(tilo_resource::Row)]
#[row(
    kind = "database.database",
    label = "Database",
    effects = DbEffect
)]
pub struct Database;

/// One schema of the open connection.
#[derive(tilo_resource::Row)]
#[row(kind = "database.schema", label = "Schema", effects = DbEffect)]
pub struct Schema;

/// One table of a schema, browsed for its rows.
#[derive(tilo_resource::Row)]
#[row(kind = "database.table", label = "Table", effects = DbEffect)]
pub struct Table;

/// One view of a schema, browsed for its rows.
#[derive(tilo_resource::Row)]
#[row(kind = "database.view", label = "View", effects = DbEffect)]
pub struct View;

/// One column of a table or view, browsed with it.
#[derive(tilo_resource::Row)]
#[row(kind = "database.column", label = "Column", effects = DbEffect)]
pub struct Column;

/// The result of the last statement this window ran.
#[derive(tilo_resource::Row)]
#[row(
    kind = "database.query-result",
    label = "Query result",
    effects = DbEffect
)]
pub struct QueryResult;

/// The open connection's transaction.
#[derive(tilo_resource::Row)]
#[row(
    kind = "database.transaction",
    label = "Transaction",
    effects = DbEffect
)]
pub struct Transaction;

/// The queries this window ran on the open connection.
#[derive(tilo_resource::Row)]
#[row(kind = "database.query-history", label = "Query history")]
pub struct QueryHistory;

/// One query in the history, run again from its row.
#[derive(tilo_resource::Row)]
#[row(
    kind = "database.query-history-entry",
    label = "Query",
    effects = DbEffect
)]
pub struct QueryHistoryEntry;

/// A statement or connection the window is still working on.
#[derive(tilo_resource::Row)]
#[row(kind = "database.job", label = "Job")]
pub struct Job;
