//! `SQLite` execution engine: opens a handle, seeds the bundled sample
//! database, introspects the schema (tables, columns, keys, indexes) and
//! runs arbitrary SQL returning a [`QueryResult`].
//!
//! The three engines — local `SQLite` plus the real network `PostgreSQL` and
//! `MySQL` drivers — are unified behind [`LiveEngine`]. All of them block, so
//! they are owned by the background [`crate::worker`] thread rather than the
//! UI thread: [`open_engine`] opens whichever engine a profile targets, and a
//! refused socket or a bad password surfaces as the driver's own error string,
//! never a fabricated success.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use rusqlite::Connection;
use rusqlite::types::ValueRef;

use crate::model::{
    Cell, ColumnInfo, ConnectionProfile, DatabaseSchema, Engine, ForeignKey, IndexInfo,
    QueryResult, RelationKind, SchemaInfo, TableInfo,
};

/// Hard cap for arbitrary query results. The grid virtualizes rendering, and
/// this cap also bounds driver-side collection so `SELECT *` cannot exhaust
/// the UI process. Table browsing remains separately paged at 100 rows.
pub const MAX_QUERY_ROWS: usize = 1_000;
/// Aggregate payload cap for one result. It prevents many columns of valid
/// (but individually capped) text from multiplying into an excessive UI heap.
pub const MAX_QUERY_RESULT_BYTES: usize = 8 * 1024 * 1024;
/// Caps vector metadata as well as payload: NULL-heavy, extremely-wide output
/// should not allocate an arbitrary number of [`Cell`] entries either.
pub const MAX_QUERY_RESULT_CELLS: usize = 100_000;
use crate::sample;

/// A private, in-memory capability for interrupting exactly the query that is
/// currently executing on the worker. It never enters a resource payload or
/// collaborative document; [`crate::worker`] pairs it with a connection id and
/// monotonically increasing request generation before invoking it.
#[derive(Clone)]
pub enum QueryCancellation {
    Sqlite(Arc<AtomicBool>),
    Postgres(postgres::CancelToken),
    MySql {
        opts: mysql::Opts,
        connection_id: u32,
    },
}

impl QueryCancellation {
    /// Start an engine-native cancellation off the UI thread. Server-side
    /// cancel requests are inherently asynchronous/racy, but the worker's
    /// generation check ensures this capability is never selected for a newer
    /// query.
    pub fn request(&self) {
        match self.clone() {
            // Not `sqlite3_interrupt`: SQLite drops an interrupt that lands
            // before the statement starts stepping, and one that lands late
            // stops whatever statement runs then. The flag belongs to this
            // query alone and its progress handler acts on it.
            Self::Sqlite(cancelled) => cancelled.store(true, Ordering::Relaxed),
            Self::Postgres(token) => {
                let _ = thread::Builder::new()
                    .name("db-pg-cancel".into())
                    .spawn(move || {
                        let _ = token.cancel_query(postgres::NoTls);
                    });
            }
            Self::MySql {
                opts,
                connection_id,
            } => {
                let _ = thread::Builder::new()
                    .name("db-mysql-cancel".into())
                    .spawn(move || {
                        let Ok(mut killer) = mysql::Conn::new(opts) else {
                            return;
                        };
                        // `connection_id` comes from the active driver's
                        // server-assigned u32, never from SQL/user input.
                        let _ = mysql::prelude::Queryable::query_drop(
                            &mut killer,
                            format!("KILL QUERY {connection_id}"),
                        );
                    });
            }
        }
    }
}

/// How many `SQLite` VM instructions run between checks of a query's
/// cancellation flag: a few microseconds of work.
const CANCELLATION_CHECK_OPS: std::ffi::c_int = 1000;

/// A live `SQLite` handle. Owns the connection for the lifetime of the
/// session; all introspection + query execution goes through it.
pub struct SqliteEngine {
    conn: Connection,
    /// The cancellation flag of the next [`run`](Self::run). Each run takes
    /// it and leaves a fresh one, so a request aimed at one query can never
    /// stop a later one.
    cancelled: Arc<AtomicBool>,
}

impl SqliteEngine {
    /// Open an in-memory database and seed it with the bundled sample
    /// schema + data so the schema browser and query runner work with no
    /// external file.
    ///
    /// # Errors
    /// Returns the driver's error string if the in-memory connection can't
    /// be opened, or if seeding the bundled sample schema/data fails.
    pub fn open_sample() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        sample::seed(&conn).map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            cancelled: Arc::default(),
        })
    }

    /// Open an on-disk `SQLite` file (read-write).
    ///
    /// # Errors
    /// Returns the driver's error string if `path` can't be opened as a
    /// `SQLite` database (missing parent directory, permission denied, or the
    /// file exists but isn't a valid `SQLite` database).
    pub fn open_file(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            cancelled: Arc::default(),
        })
    }

    /// Introspect the full schema. `SQLite` has a single implicit namespace,
    /// which we surface as a "main" schema so the tree shape matches the
    /// multi-schema engines.
    pub fn introspect(&self) -> Result<DatabaseSchema, String> {
        let mut tables = Vec::new();

        // Enumerate user tables + views, skipping SQLite's internal tables.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT name, type FROM sqlite_master \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' \
                 ORDER BY type DESC, name",
            )
            .map_err(|e| e.to_string())?;
        let relations: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);

        for (name, kind) in relations {
            let kind = if kind == "view" {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            let columns = self.table_columns(&name)?;
            let foreign_keys = self.table_foreign_keys(&name)?;
            let indexes = self.table_indexes(&name)?;
            let row_count = if kind == RelationKind::Table {
                self.count_rows(&name).ok()
            } else {
                None
            };
            tables.push(TableInfo {
                name,
                kind,
                columns,
                foreign_keys,
                indexes,
                row_count,
            });
        }

        Ok(DatabaseSchema {
            schemas: vec![SchemaInfo {
                name: "main".to_string(),
                tables,
            }],
        })
    }

    fn table_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, String> {
        let sql = format!("PRAGMA table_info(\"{}\")", table.replace('"', "\"\""));
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let cols = stmt
            .query_map([], |r| {
                let decl: String = r.get(2)?;
                let not_null: i64 = r.get(3)?;
                let default: Option<String> = r.get(4)?;
                let pk: i64 = r.get(5)?;
                Ok(ColumnInfo {
                    name: r.get(1)?,
                    decl_type: decl,
                    not_null: not_null != 0,
                    primary_key: pk != 0,
                    default,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(cols)
    }

    fn table_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, String> {
        let sql = format!(
            "PRAGMA foreign_key_list(\"{}\")",
            table.replace('"', "\"\"")
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let fks = stmt
            .query_map([], |r| {
                Ok(ForeignKey {
                    ref_table: r.get(2)?,
                    column: r.get(3)?,
                    ref_column: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(fks)
    }

    fn table_indexes(&self, table: &str) -> Result<Vec<IndexInfo>, String> {
        let sql = format!("PRAGMA index_list(\"{}\")", table.replace('"', "\"\""));
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows: Vec<(String, bool)> = stmt
            .query_map([], |r| {
                let name: String = r.get(1)?;
                let unique: i64 = r.get(2)?;
                Ok((name, unique != 0))
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);

        let mut out = Vec::new();
        for (name, unique) in rows {
            if name.starts_with("sqlite_autoindex") {
                continue;
            }
            let info_sql = format!("PRAGMA index_info(\"{}\")", name.replace('"', "\"\""));
            let mut s2 = self.conn.prepare(&info_sql).map_err(|e| e.to_string())?;
            let columns: Vec<String> = s2
                .query_map([], |r| r.get::<_, String>(2))
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .collect();
            out.push(IndexInfo {
                name,
                unique,
                columns,
            });
        }
        Ok(out)
    }

    fn count_rows(&self, table: &str) -> Result<i64, String> {
        let sql = format!("SELECT COUNT(*) FROM \"{}\"", table.replace('"', "\"\""));
        self.conn
            .query_row(&sql, [], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())
    }

    /// Run a single SQL statement and collect the result.
    ///
    /// # Errors
    /// Returns the driver's error string if `sql` fails to prepare or
    /// execute, or if a returned row's column value can't be read.
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        let cancelled = std::mem::take(&mut self.cancelled);
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult::default());
        }
        self.conn.progress_handler(
            CANCELLATION_CHECK_OPS,
            Some(move || cancelled.load(Ordering::Relaxed)),
        );
        let result = self.run_statement(trimmed);
        self.conn.progress_handler(0, None::<fn() -> bool>);
        result
    }

    fn run_statement(&self, trimmed: &str) -> Result<QueryResult, String> {
        let start = ui::time::now();

        // Decide select-vs-DML by trying to prepare and asking the column
        // count. A zero-column statement is DML/DDL.
        let mut stmt = self.conn.prepare(trimmed).map_err(|e| e.to_string())?;
        let col_count = stmt.column_count();

        if col_count == 0 {
            // DML/DDL — execute and report affected rows.
            let affected = self.conn.execute(trimmed, []).map_err(|e| e.to_string())?;
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
                rows_affected: Some(affected as i64),
                is_select: false,
                truncation: None,
            });
        }

        let columns: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();

        let mut rows_out: Vec<Vec<Cell>> = Vec::new();
        let mut retained_bytes = 0usize;
        let mut retained_cells = 0usize;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut truncation: Option<crate::model::Truncation> = None;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if rows_out.len() == MAX_QUERY_ROWS {
                truncation = Some(crate::model::Truncation::RowLimit);
                break;
            }
            let mut cells = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v = row.get_ref(i).map_err(|e| e.to_string())?;
                cells.push(value_ref_to_cell(v));
            }
            if !can_retain_result_row(retained_bytes, retained_cells, &cells) {
                truncation = Some(crate::model::Truncation::Payload);
                break;
            }
            retained_bytes += row_retained_bytes(&cells);
            retained_cells += cells.len();
            rows_out.push(cells);
        }

        Ok(QueryResult {
            columns,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            rows_affected: None,
            is_select: true,
            rows: rows_out,
            truncation,
        })
    }

    pub(crate) fn cancellation_handle(&self) -> QueryCancellation {
        QueryCancellation::Sqlite(Arc::clone(&self.cancelled))
    }
}

fn value_ref_to_cell(v: ValueRef<'_>) -> Cell {
    match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => Cell::Integer(i),
        ValueRef::Real(r) => Cell::Real(r),
        ValueRef::Text(t) => Cell::bounded_text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Cell::Blob(b.len()),
    }
}

/// Whether adding `row` stays inside both aggregate result limits. Every
/// engine calls this before retaining a decoded row, keeping result memory
/// bounded regardless of backend or result shape.
pub(crate) fn can_retain_result_row(
    retained_bytes: usize,
    retained_cells: usize,
    row: &[Cell],
) -> bool {
    retained_bytes.saturating_add(row_retained_bytes(row)) <= MAX_QUERY_RESULT_BYTES
        && retained_cells.saturating_add(row.len()) <= MAX_QUERY_RESULT_CELLS
}

pub(crate) fn row_retained_bytes(row: &[Cell]) -> usize {
    row.iter().map(Cell::retained_bytes).sum()
}

/// A live connection to any supported engine. The session holds one of these
/// and dispatches introspection + query execution uniformly, so `SQLite` (local,
/// bundled) and the real network engines share one code path.
pub enum LiveEngine {
    Sqlite(SqliteEngine),
    Postgres(crate::engine_pg::PostgresEngine),
    MySql(crate::engine_mysql::MySqlEngine),
}

impl LiveEngine {
    /// Introspect the full schema for whichever engine backs this handle.
    pub fn introspect(&mut self) -> Result<DatabaseSchema, String> {
        match self {
            LiveEngine::Sqlite(e) => e.introspect(),
            LiveEngine::Postgres(e) => e.introspect(),
            LiveEngine::MySql(e) => e.introspect(),
        }
    }

    /// Run a single SQL statement against whichever engine backs this handle.
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        match self {
            LiveEngine::Sqlite(e) => e.run(sql),
            LiveEngine::Postgres(e) => e.run(sql),
            LiveEngine::MySql(e) => e.run(sql),
        }
    }

    /// Capture the backend-specific interruption capability before beginning
    /// a query. The worker retains it only for that query generation.
    pub(crate) fn cancellation_handle(&self) -> QueryCancellation {
        match self {
            LiveEngine::Sqlite(e) => e.cancellation_handle(),
            LiveEngine::Postgres(e) => e.cancellation_handle(),
            LiveEngine::MySql(e) => e.cancellation_handle(),
        }
    }

    /// Read one page of a table's rows. The identifier is quoted the way the
    /// backing engine expects — double quotes for SQLite/PostgreSQL, backticks
    /// for `MySQL` — so a table named like a keyword (or a `MySQL` table at all)
    /// browses correctly rather than erroring on the wrong quote style.
    pub fn browse_table(
        &mut self,
        schema: &str,
        table: &str,
        limit: usize,
        offset: usize,
    ) -> Result<QueryResult, String> {
        let sql = match self {
            LiveEngine::MySql(_) => format!(
                "SELECT * FROM `{}`.`{}` LIMIT {} OFFSET {}",
                schema.replace('`', "``"),
                table.replace('`', "``"),
                limit,
                offset
            ),
            _ => format!(
                "SELECT * FROM \"{}\".\"{}\" LIMIT {} OFFSET {}",
                schema.replace('"', "\"\""),
                table.replace('"', "\"\""),
                limit,
                offset
            ),
        };
        self.run(&sql)
    }
}

/// Open the live engine a profile targets. Every engine connects for real:
/// the bundled sample or an on-disk file for `SQLite`, a real TCP socket for
/// `PostgreSQL` and `MySQL`. This is the single seam the [`crate::worker`] thread
/// calls; it never runs on the UI thread because each variant blocks.
pub fn open_engine(profile: &ConnectionProfile) -> Result<LiveEngine, String> {
    if profile.is_sample {
        return SqliteEngine::open_sample().map(LiveEngine::Sqlite);
    }
    match profile.engine {
        Engine::Sqlite if profile.file_path.is_empty() => {
            Err("SQLite connection has no file path".to_string())
        }
        Engine::Sqlite => SqliteEngine::open_file(&profile.file_path).map(LiveEngine::Sqlite),
        Engine::Postgres => {
            crate::engine_pg::PostgresEngine::connect(profile).map(LiveEngine::Postgres)
        }
        Engine::MySql => crate::engine_mysql::MySqlEngine::connect(profile).map(LiveEngine::MySql),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `SQLite` engine introspects tables and foreign keys, returns rows and reports affected rows.
    #[test]
    fn sqlite_engine_introspects_and_executes() {
        // --- phase: sample_engine_introspects_expected_tables ---
        {
            let eng = SqliteEngine::open_sample().unwrap();
            let schema = eng.introspect().unwrap();
            assert_eq!(schema.schemas.len(), 1);
            assert!(schema.table_count() >= 4, "got {}", schema.table_count());
            let artists = schema.find_table("artists").expect("artists table");
            assert!(artists.columns.iter().any(|c| c.primary_key));
        }

        // --- phase: select_returns_rows_with_columns ---
        {
            let mut eng = SqliteEngine::open_sample().unwrap();
            let r = eng
                .run("SELECT name FROM artists ORDER BY name LIMIT 3")
                .unwrap();
            assert!(r.is_select);
            assert_eq!(r.columns, vec!["name".to_string()]);
            assert_eq!(r.rows.len(), 3);
        }

        // --- phase: dml_reports_rows_affected ---
        {
            let mut eng = SqliteEngine::open_sample().unwrap();
            let r = eng
                .run("UPDATE tracks SET unit_price = unit_price WHERE 1=0")
                .unwrap();
            assert!(!r.is_select);
            assert_eq!(r.rows_affected, Some(0));
        }

        // --- phase: foreign_keys_are_discovered ---
        {
            let eng = SqliteEngine::open_sample().unwrap();
            let schema = eng.introspect().unwrap();
            let albums = schema.find_table("albums").unwrap();
            assert!(
                albums.foreign_keys.iter().any(|f| f.ref_table == "artists"),
                "albums should reference artists"
            );
        }
    }

    /// Hostile text and many large cells are truncated to bounded sizes.
    #[test]
    fn sqlite_results_are_bounded() {
        // --- phase: sqlite_hostile_text_is_retained_at_a_bounded_size ---
        {
            let mut eng = SqliteEngine::open_sample().unwrap();
            let sql = format!(
                "SELECT '{}' AS hostile",
                "x".repeat(Cell::MAX_RETAINED_TEXT_BYTES + 99)
            );
            let result = eng.run(&sql).unwrap();
            let Cell::Text(value) = &result.rows[0][0] else {
                panic!("text result expected")
            };
            assert!(value.len() <= Cell::MAX_RETAINED_TEXT_BYTES + 32);
            assert!(value.ends_with("… [truncated]"));
        }

        // --- phase: aggregate_budget_truncates_many_large_cells_across_rows_and_columns ---
        {
            let mut eng = SqliteEngine::open_sample().unwrap();
            let text = "x".repeat(Cell::MAX_RETAINED_TEXT_BYTES);
            let columns = (0..8)
                .map(|index| format!("'{text}' AS c{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let result = eng
            .run(&format!(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 100) \
                 SELECT {columns} FROM n"
            ))
            .unwrap();
            assert_eq!(result.truncation, Some(crate::model::Truncation::Payload));
            assert!(result.rows.len() < 100);
            let retained: usize = result
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .map(Cell::retained_bytes)
                .sum();
            assert!(retained <= MAX_QUERY_RESULT_BYTES);
        }
    }

    /// A cancellation requested before the statement starts still stops it:
    /// `SQLite` forgets an interrupt that lands before the first step, which
    /// is exactly when a quick Cancel after Run arrives.
    #[test]
    fn sqlite_cancellation_before_the_statement_starts_still_stops_it() {
        let mut eng = SqliteEngine::open_sample().unwrap();
        eng.cancellation_handle().request();
        let result = eng.run(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 1000000000000) \
             SELECT sum(x) FROM n",
        );
        assert!(
            result.is_err(),
            "the cancelled query must stop, got {result:?}"
        );

        // The request belonged to that query only; the next one runs.
        let next = eng.run("SELECT 1").unwrap();
        assert_eq!(next.rows.len(), 1);
    }
}
