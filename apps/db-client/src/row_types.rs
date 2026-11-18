//! The types the catalogue's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource.

use ui::tilo_resource::words::RowType;

pub(crate) struct Database;

impl RowType for Database {
    const KIND: &'static str = "database.database";
    const LABEL: &'static str = "Database";
}

pub(crate) struct Schema;

impl RowType for Schema {
    const KIND: &'static str = "database.schema";
    const LABEL: &'static str = "Schema";
}

pub(crate) struct Table;

impl RowType for Table {
    const KIND: &'static str = "database.table";
    const LABEL: &'static str = "Table";
}

pub(crate) struct View;

impl RowType for View {
    const KIND: &'static str = "database.view";
    const LABEL: &'static str = "View";
}

pub(crate) struct Column;

impl RowType for Column {
    const KIND: &'static str = "database.column";
    const LABEL: &'static str = "Column";
}

pub(crate) struct QueryResult;

impl RowType for QueryResult {
    const KIND: &'static str = "database.query-result";
    const LABEL: &'static str = "Query result";
}

pub(crate) struct Transaction;

impl RowType for Transaction {
    const KIND: &'static str = "database.transaction";
    const LABEL: &'static str = "Transaction";
}

pub(crate) struct QueryHistory;

impl RowType for QueryHistory {
    const KIND: &'static str = "database.query-history";
    const LABEL: &'static str = "Query history";
}

pub(crate) struct QueryHistoryEntry;

impl RowType for QueryHistoryEntry {
    const KIND: &'static str = "database.query-history-entry";
    const LABEL: &'static str = "Query";
}
