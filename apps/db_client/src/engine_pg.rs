//! `PostgreSQL` execution engine: opens a real TCP connection through the sync
//! `postgres` crate, introspects the catalog (`information_schema` +
//! `pg_catalog`) and runs arbitrary SQL returning a [`QueryResult`]. Mirrors
//! the shape of [`crate::engine::SqliteEngine`] so the session layer can hold
//! any engine behind one enum.
//!
//! Values are read over the simple-query text protocol, so every server type
//! (including `numeric`, `json`, arrays, …) round-trips as its own canonical
//! text form with no per-type `FromSql` gaps. Column names and the
//! SELECT-vs-DML split come from a prepared statement, so empty result sets
//! still carry their headers and DML reports rows-affected.

use std::time::{Duration, Instant};

use postgres::{Client, Config, NoTls, SimpleQueryMessage};

use crate::model::{
    Cell, ColumnInfo, DatabaseSchema, ForeignKey, IndexInfo, QueryResult, RelationKind, SchemaInfo,
    TableInfo, TypeClass,
};

/// A live `PostgreSQL` connection. Owns the client for the session lifetime.
pub struct PostgresEngine {
    client: Client,
}

/// Build a [`Config`] from a connection profile + password. Pure and
/// side-effect free so it can be unit-tested without a server.
pub(crate) fn pg_config(profile: &crate::model::ConnectionProfile) -> Config {
    let mut cfg = Config::new();
    let host = if profile.host.is_empty() {
        "localhost"
    } else {
        profile.host.as_str()
    };
    cfg.host(host);
    cfg.port(if profile.port == 0 {
        5432
    } else {
        profile.port
    });
    if !profile.user.is_empty() {
        cfg.user(&profile.user);
    }
    if !profile.password.is_empty() {
        cfg.password(&profile.password);
    }
    if !profile.database.is_empty() {
        cfg.dbname(&profile.database);
    }
    cfg.connect_timeout(Duration::from_secs(10));
    cfg.application_name("qos-db-client");
    cfg
}

impl PostgresEngine {
    /// Open a real connection. A refused socket or a failed auth surfaces as
    /// the driver's own error string — never a fabricated success.
    pub fn connect(profile: &crate::model::ConnectionProfile) -> Result<Self, String> {
        let cfg = pg_config(profile);
        let client = cfg.connect(NoTls).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    /// Introspect every user schema (skipping `pg_catalog` /
    /// `information_schema`) into the shared [`DatabaseSchema`] shape.
    pub fn introspect(&mut self) -> Result<DatabaseSchema, String> {
        let rows = self
            .client
            .query(
                "SELECT table_schema, table_name, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY table_schema, table_name",
                &[],
            )
            .map_err(|e| e.to_string())?;

        // (schema, table, type) in catalog order.
        let mut entries: Vec<(String, String, String)> = Vec::new();
        for row in &rows {
            let schema: String = row.try_get(0).map_err(|e| e.to_string())?;
            let table: String = row.try_get(1).map_err(|e| e.to_string())?;
            let ttype: String = row.try_get(2).map_err(|e| e.to_string())?;
            entries.push((schema, table, ttype));
        }

        let mut schemas: Vec<SchemaInfo> = Vec::new();
        for (schema, table, ttype) in entries {
            let kind = if ttype.eq_ignore_ascii_case("VIEW") {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            let columns = self.columns(&schema, &table)?;
            let foreign_keys = self.foreign_keys(&schema, &table)?;
            let indexes = self.indexes(&schema, &table)?;
            let row_count = if kind == RelationKind::Table {
                self.count_rows(&schema, &table).ok()
            } else {
                None
            };
            let info = TableInfo {
                name: table,
                kind,
                columns,
                foreign_keys,
                indexes,
                row_count,
            };
            match schemas.iter_mut().find(|s| s.name == schema) {
                Some(s) => s.tables.push(info),
                None => schemas.push(SchemaInfo {
                    name: schema,
                    tables: vec![info],
                }),
            }
        }

        Ok(DatabaseSchema { schemas })
    }

    fn columns(&mut self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, String> {
        // Primary-key column set first, folded into each column's flag.
        let pk_rows = self
            .client
            .query(
                "SELECT kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.constraint_schema = kcu.constraint_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = $1 AND tc.table_name = $2",
                &[&schema, &table],
            )
            .map_err(|e| e.to_string())?;
        let pks: Vec<String> = pk_rows
            .iter()
            .filter_map(|r| r.try_get::<_, String>(0).ok())
            .collect();

        let rows = self
            .client
            .query(
                "SELECT column_name, data_type, is_nullable, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2 \
                 ORDER BY ordinal_position",
                &[&schema, &table],
            )
            .map_err(|e| e.to_string())?;

        let mut cols = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get(0).map_err(|e| e.to_string())?;
            let decl: String = row.try_get(1).map_err(|e| e.to_string())?;
            let nullable: String = row.try_get(2).map_err(|e| e.to_string())?;
            let default: Option<String> = row.try_get(3).ok().flatten();
            cols.push(ColumnInfo {
                class: TypeClass::classify(&decl),
                primary_key: pks.iter().any(|p| p == &name),
                not_null: nullable.eq_ignore_ascii_case("NO"),
                decl_type: decl,
                name,
                default,
            });
        }
        Ok(cols)
    }

    fn foreign_keys(&mut self, schema: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
        let rows = self
            .client
            .query(
                "SELECT kcu.column_name, ccu.table_name, ccu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.constraint_schema = kcu.constraint_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name \
                  AND ccu.constraint_schema = tc.constraint_schema \
                 WHERE tc.constraint_type = 'FOREIGN KEY' \
                   AND tc.table_schema = $1 AND tc.table_name = $2",
                &[&schema, &table],
            )
            .map_err(|e| e.to_string())?;
        let mut fks = Vec::new();
        for row in &rows {
            fks.push(ForeignKey {
                column: row.try_get(0).map_err(|e| e.to_string())?,
                ref_table: row.try_get(1).map_err(|e| e.to_string())?,
                ref_column: row.try_get(2).map_err(|e| e.to_string())?,
            });
        }
        Ok(fks)
    }

    fn indexes(&mut self, schema: &str, table: &str) -> Result<Vec<IndexInfo>, String> {
        let rows = self
            .client
            .query(
                "SELECT i.relname, ix.indisunique, a.attname \
                 FROM pg_class t \
                 JOIN pg_namespace n ON n.oid = t.relnamespace \
                 JOIN pg_index ix ON t.oid = ix.indrelid \
                 JOIN pg_class i ON i.oid = ix.indexrelid \
                 JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = ANY(ix.indkey) \
                 WHERE n.nspname = $1 AND t.relname = $2 \
                 ORDER BY i.relname, array_position(ix.indkey, a.attnum)",
                &[&schema, &table],
            )
            .map_err(|e| e.to_string())?;

        let mut out: Vec<IndexInfo> = Vec::new();
        for row in &rows {
            let name: String = row.try_get(0).map_err(|e| e.to_string())?;
            let unique: bool = row.try_get(1).map_err(|e| e.to_string())?;
            let column: String = row.try_get(2).map_err(|e| e.to_string())?;
            match out.iter_mut().find(|i| i.name == name) {
                Some(i) => i.columns.push(column),
                None => out.push(IndexInfo {
                    name,
                    unique,
                    columns: vec![column],
                }),
            }
        }
        Ok(out)
    }

    fn count_rows(&mut self, schema: &str, table: &str) -> Result<i64, String> {
        let sql = format!(
            "SELECT COUNT(*) FROM \"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            table.replace('"', "\"\"")
        );
        let row = self
            .client
            .query_one(&sql, &[])
            .map_err(|e| e.to_string())?;
        row.try_get::<_, i64>(0).map_err(|e| e.to_string())
    }

    /// Run a single SQL statement. Uses a prepared statement to learn column
    /// names + whether the statement returns rows, then the text protocol to
    /// read the values. Falls back to a pure simple-query path for inputs the
    /// extended protocol rejects (e.g. multiple `;`-separated statements).
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult::default());
        }
        let start = Instant::now();

        match self.client.prepare(trimmed) {
            Ok(stmt) if stmt.columns().is_empty() => {
                // DML/DDL — execute and report affected rows.
                let affected = self.client.execute(&stmt, &[]).map_err(|e| e.to_string())?;
                Ok(QueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                    elapsed_ms: elapsed_ms(start),
                    rows_affected: Some(affected as i64),
                    is_select: false,
                })
            }
            Ok(stmt) => {
                let columns: Vec<String> = stmt
                    .columns()
                    .iter()
                    .map(|c| c.name().to_string())
                    .collect();
                let rows = self.simple_rows(trimmed)?;
                Ok(QueryResult {
                    columns,
                    rows,
                    elapsed_ms: elapsed_ms(start),
                    rows_affected: None,
                    is_select: true,
                })
            }
            Err(_) => self.run_simple(trimmed, start),
        }
    }

    /// Collect the row values of a row-returning statement over the text
    /// protocol.
    fn simple_rows(&mut self, sql: &str) -> Result<Vec<Vec<Cell>>, String> {
        let messages = self.client.simple_query(sql).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let mut cells = Vec::with_capacity(row.len());
                for i in 0..row.len() {
                    cells.push(simple_value_to_cell(row.get(i)));
                }
                rows.push(cells);
            }
        }
        Ok(rows)
    }

    /// Fallback path used when the extended protocol can't prepare the input
    /// (multi-statement scripts). Column names come from the first returned
    /// row; DML reports affected rows from `CommandComplete`.
    fn run_simple(&mut self, sql: &str, start: Instant) -> Result<QueryResult, String> {
        let messages = self.client.simple_query(sql).map_err(|e| e.to_string())?;
        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut affected: i64 = 0;
        let mut saw_rows = false;
        for msg in messages {
            match msg {
                SimpleQueryMessage::Row(row) => {
                    saw_rows = true;
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    let mut cells = Vec::with_capacity(row.len());
                    for i in 0..row.len() {
                        cells.push(simple_value_to_cell(row.get(i)));
                    }
                    rows.push(cells);
                }
                SimpleQueryMessage::CommandComplete(n) => affected += n as i64,
                _ => {}
            }
        }
        let is_select = saw_rows || is_row_returning(sql);
        Ok(QueryResult {
            columns,
            rows,
            elapsed_ms: elapsed_ms(start),
            rows_affected: if is_select { None } else { Some(affected) },
            is_select,
        })
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Map a text-protocol value to a [`Cell`]. Postgres already encodes every
/// type into its canonical text form, so a present value is honest text and an
/// absent one is a real NULL.
pub(crate) fn simple_value_to_cell(v: Option<&str>) -> Cell {
    match v {
        None => Cell::Null,
        Some(s) => Cell::Text(s.to_string()),
    }
}

/// Heuristic used only on the multi-statement fallback path: does the leading
/// keyword denote a row-returning statement?
fn is_row_returning(sql: &str) -> bool {
    let head = sql
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "EXPLAIN"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};

    #[test]
    fn config_carries_profile_fields() {
        let mut p = ConnectionProfile::new("prod", Engine::Postgres);
        p.host = "db.example".into();
        p.port = 6543;
        p.database = "analytics".into();
        p.user = "reader".into();
        p.password = "secret".into();
        let cfg = pg_config(&p);
        assert_eq!(cfg.get_ports(), &[6543]);
        assert_eq!(cfg.get_user(), Some("reader"));
        assert_eq!(cfg.get_dbname(), Some("analytics"));
        // Host list carries the TCP host.
        assert!(
            cfg.get_hosts()
                .iter()
                .any(|h| matches!(h, postgres::config::Host::Tcp(t) if t == "db.example")),
            "host not set on config"
        );
    }

    #[test]
    fn config_defaults_empty_port_to_5432() {
        let mut p = ConnectionProfile::new("x", Engine::Postgres);
        p.port = 0;
        p.host = String::new();
        let cfg = pg_config(&p);
        assert_eq!(cfg.get_ports(), &[5432]);
    }

    #[test]
    fn simple_value_maps_null_and_text() {
        assert_eq!(simple_value_to_cell(None), Cell::Null);
        assert_eq!(simple_value_to_cell(Some("42")), Cell::Text("42".into()));
        assert_eq!(simple_value_to_cell(Some("")), Cell::Text(String::new()));
    }

    #[test]
    fn row_returning_detects_select_family() {
        assert!(is_row_returning("SELECT 1"));
        assert!(is_row_returning("  with x as (select 1) select * from x"));
        assert!(is_row_returning("VALUES (1),(2)"));
        assert!(!is_row_returning("INSERT INTO t VALUES (1)"));
        assert!(!is_row_returning("UPDATE t SET a=1"));
        assert!(!is_row_returning("CREATE TABLE t(a int)"));
    }
}
