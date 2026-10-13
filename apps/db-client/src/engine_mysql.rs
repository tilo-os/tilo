//! `MySQL` / `MariaDB` execution engine: opens a real TCP connection through the
//! sync `mysql` crate, introspects `information_schema` and runs arbitrary SQL
//! returning a [`QueryResult`]. Mirrors [`crate::engine::SqliteEngine`] and
//! [`crate::engine_pg::PostgresEngine`] so the session can hold any engine
//! behind one enum.

use ui::time::{Duration, Instant};

use mysql::consts::ColumnType;
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Value};

use crate::engine::{MAX_QUERY_ROWS, can_retain_result_row, row_retained_bytes};
use crate::model::{
    Cell, ColumnInfo, DatabaseSchema, ForeignKey, IndexInfo, QueryResult, RelationKind, SchemaInfo,
    TableInfo,
};

/// A live `MySQL` connection. Owns the connection for the session lifetime.
pub struct MySqlEngine {
    conn: Conn,
    opts: Opts,
}

/// Build the connection options from a profile + password. Pure so the
/// mapping is unit-testable without a server.
pub(crate) fn mysql_opts(profile: &crate::model::ConnectionProfile) -> OptsBuilder {
    let host = if profile.host.is_empty() {
        "localhost".to_string()
    } else {
        profile.host.clone()
    };
    let mut b = OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(if profile.port == 0 {
            3306
        } else {
            profile.port
        })
        .tcp_connect_timeout(Some(Duration::from_secs(10)));
    if !profile.user.is_empty() {
        b = b.user(Some(profile.user.clone()));
    }
    if !profile.password.is_empty() {
        b = b.pass(Some(profile.password.clone()));
    }
    if !profile.database.is_empty() {
        b = b.db_name(Some(profile.database.clone()));
    }
    b
}

impl MySqlEngine {
    /// Open a real connection. A refused socket or a failed auth surfaces as
    /// the driver's own error string — never a fabricated success.
    ///
    /// # Errors
    /// Returns the driver's error string if the TCP connection is refused or
    /// authentication fails.
    pub fn connect(profile: &crate::model::ConnectionProfile) -> Result<Self, String> {
        let opts: Opts = mysql_opts(profile).into();
        let conn = Conn::new(opts.clone()).map_err(|e| e.to_string())?;
        Ok(Self { conn, opts })
    }

    pub(crate) fn cancellation_handle(&self) -> crate::engine::QueryCancellation {
        crate::engine::QueryCancellation::MySql {
            opts: self.opts.clone(),
            connection_id: self.conn.connection_id(),
        }
    }

    /// Introspect the connection's current database into the shared
    /// [`DatabaseSchema`] shape. `MySQL`'s schema namespace is the database, so
    /// this yields a single [`SchemaInfo`] named after it.
    ///
    /// # Errors
    /// Returns the driver's error string if any of the underlying
    /// `information_schema` queries (listing tables, or a table's columns,
    /// foreign keys or indexes) fail.
    pub fn introspect(&mut self) -> Result<DatabaseSchema, String> {
        let db: Option<String> = self
            .conn
            .query_first::<Option<String>, _>("SELECT DATABASE()")
            .map_err(|e| e.to_string())?
            .flatten();
        let schema_name = db.unwrap_or_else(|| "mysql".to_string());

        let relations: Vec<(String, String)> = self
            .conn
            .query_map(
                "SELECT table_name, table_type FROM information_schema.tables \
                 WHERE table_schema = DATABASE() ORDER BY table_name",
                |(name, ttype): (String, String)| (name, ttype),
            )
            .map_err(|e| e.to_string())?;

        let mut tables = Vec::with_capacity(relations.len());
        for (name, ttype) in relations {
            let kind = if ttype.eq_ignore_ascii_case("VIEW") {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            let columns = self.columns(&name)?;
            let foreign_keys = self.foreign_keys(&name)?;
            let indexes = self.indexes(&name)?;
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
                name: schema_name,
                tables,
            }],
        })
    }

    fn columns(&mut self, table: &str) -> Result<Vec<ColumnInfo>, String> {
        let rows = self
            .conn
            .exec_map(
                "SELECT column_name, column_type, is_nullable, column_key, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
                (table,),
                |(name, ctype, nullable, key, default): (
                    String,
                    String,
                    String,
                    String,
                    Option<String>,
                )| {
                    ColumnInfo {
                        primary_key: key.eq_ignore_ascii_case("PRI"),
                        not_null: nullable.eq_ignore_ascii_case("NO"),
                        decl_type: ctype,
                        name,
                        default,
                    }
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    fn foreign_keys(&mut self, table: &str) -> Result<Vec<ForeignKey>, String> {
        let rows = self
            .conn
            .exec_map(
                "SELECT column_name, referenced_table_name, referenced_column_name \
                 FROM information_schema.key_column_usage \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                   AND referenced_table_name IS NOT NULL",
                (table,),
                |(column, ref_table, ref_column): (String, String, String)| ForeignKey {
                    column,
                    ref_table,
                    ref_column,
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    fn indexes(&mut self, table: &str) -> Result<Vec<IndexInfo>, String> {
        let rows: Vec<(String, i64, String)> = self
            .conn
            .exec_map(
                "SELECT index_name, non_unique, column_name \
                 FROM information_schema.statistics \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY index_name, seq_in_index",
                (table,),
                |(name, non_unique, column): (String, i64, String)| (name, non_unique, column),
            )
            .map_err(|e| e.to_string())?;

        let mut out: Vec<IndexInfo> = Vec::new();
        for (name, non_unique, column) in rows {
            match out.iter_mut().find(|i| i.name == name) {
                Some(i) => i.columns.push(column),
                None => out.push(IndexInfo {
                    name,
                    unique: non_unique == 0,
                    columns: vec![column],
                }),
            }
        }
        Ok(out)
    }

    fn count_rows(&mut self, table: &str) -> Result<i64, String> {
        let sql = format!("SELECT COUNT(*) FROM `{}`", table.replace('`', "``"));
        let n: Option<i64> = self.conn.query_first(&sql).map_err(|e| e.to_string())?;
        Ok(n.unwrap_or(0))
    }

    /// Run a single SQL statement and collect the result. Empty column list
    /// means the statement was DML/DDL — reported via affected rows.
    ///
    /// # Errors
    /// Returns the driver's error string if `sql` fails to execute, or if a
    /// row can't be read back from the connection.
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult::default());
        }
        let start = ui::time::now();

        let mut result = self.conn.query_iter(trimmed).map_err(|e| e.to_string())?;
        // Capture the column names + server types. The text protocol returns
        // every value as raw bytes, so the type is what lets an integer column
        // land as a typed numeric cell (and right-align in the grid) rather
        // than as a text string.
        let col_meta: Vec<(String, ColumnType)> = result
            .columns()
            .as_ref()
            .iter()
            .map(|c| (c.name_str().to_string(), c.column_type()))
            .collect();
        let columns: Vec<String> = col_meta.iter().map(|(n, _)| n.clone()).collect();

        if columns.is_empty() {
            let affected = result.affected_rows() as i64;
            drop(result);
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                elapsed_ms: elapsed_ms(start),
                rows_affected: Some(affected),
                is_select: false,
                truncated: false,
            });
        }

        let mut rows_out: Vec<Vec<Cell>> = Vec::new();
        let mut truncated = false;
        let mut retained_bytes = 0usize;
        let mut retained_cells = 0usize;
        for row_res in result.by_ref() {
            if rows_out.len() == MAX_QUERY_ROWS {
                truncated = true;
                break;
            }
            let row = row_res.map_err(|e| e.to_string())?;
            let values: Vec<Value> = row.unwrap();
            let cells: Vec<Cell> = values
                .into_iter()
                .enumerate()
                .map(|(i, v)| cell_from_value(v, col_meta.get(i).map(|(_, ct)| *ct)))
                .collect();
            if !can_retain_result_row(retained_bytes, retained_cells, &cells) {
                truncated = true;
                break;
            }
            retained_bytes += row_retained_bytes(&cells);
            retained_cells += cells.len();
            rows_out.push(cells);
        }
        drop(result);

        Ok(QueryResult {
            columns,
            rows: rows_out,
            elapsed_ms: elapsed_ms(start),
            rows_affected: None,
            is_select: true,
            truncated,
        })
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Map a text-protocol value to a [`Cell`], using the column's server type to
/// recover typed numerics. `query_iter` returns every value as raw bytes, so
/// an integer/float column arrives as `Value::Bytes`; parsing it back to a
/// typed cell keeps the grid's numeric right-alignment honest. Non-numeric
/// columns and binary-protocol values fall through to [`mysql_value_to_cell`].
pub(crate) fn cell_from_value(v: Value, col_type: Option<ColumnType>) -> Cell {
    let Value::Bytes(bytes) = v else {
        return mysql_value_to_cell(v);
    };
    let s = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => return Cell::Blob(e.into_bytes().len()),
    };
    match col_type {
        Some(ct) if is_integer_type(ct) => s
            .parse::<i64>()
            .map_or_else(|_| Cell::bounded_text(s), Cell::Integer),
        Some(ct) if is_float_type(ct) => s
            .parse::<f64>()
            .map_or_else(|_| Cell::bounded_text(s), Cell::Real),
        _ => Cell::bounded_text(s),
    }
}

fn is_integer_type(ct: ColumnType) -> bool {
    matches!(
        ct,
        ColumnType::MYSQL_TYPE_TINY
            | ColumnType::MYSQL_TYPE_SHORT
            | ColumnType::MYSQL_TYPE_LONG
            | ColumnType::MYSQL_TYPE_LONGLONG
            | ColumnType::MYSQL_TYPE_INT24
            | ColumnType::MYSQL_TYPE_YEAR
    )
}

fn is_float_type(ct: ColumnType) -> bool {
    matches!(
        ct,
        ColumnType::MYSQL_TYPE_FLOAT | ColumnType::MYSQL_TYPE_DOUBLE
    )
}

/// Map a driver [`Value`] to a [`Cell`]. Integers/floats keep their type;
/// byte strings become text when valid UTF-8, otherwise a blob length;
/// temporal values render as their canonical string form.
pub(crate) fn mysql_value_to_cell(v: Value) -> Cell {
    match v {
        Value::NULL => Cell::Null,
        Value::Int(i) => Cell::Integer(i),
        Value::UInt(u) => {
            if i64::try_from(u).is_ok() {
                Cell::Integer(u as i64)
            } else {
                Cell::bounded_text(u.to_string())
            }
        }
        Value::Float(f) => Cell::Real(f64::from(f)),
        Value::Double(d) => Cell::Real(d),
        Value::Bytes(b) => match String::from_utf8(b) {
            Ok(s) => Cell::bounded_text(s),
            Err(e) => Cell::Blob(e.into_bytes().len()),
        },
        Value::Date(year, month, day, hour, min, sec, micros) => {
            if hour == 0 && min == 0 && sec == 0 && micros == 0 {
                Cell::bounded_text(format!("{year:04}-{month:02}-{day:02}"))
            } else if micros == 0 {
                Cell::bounded_text(format!(
                    "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}"
                ))
            } else {
                Cell::bounded_text(format!(
                    "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}.{micros:06}"
                ))
            }
        }
        Value::Time(neg, days, hour, min, sec, micros) => {
            let sign = if neg { "-" } else { "" };
            let hours = days * 24 + u32::from(hour);
            if micros == 0 {
                Cell::bounded_text(format!("{sign}{hours:02}:{min:02}:{sec:02}"))
            } else {
                Cell::bounded_text(format!("{sign}{hours:02}:{min:02}:{sec:02}.{micros:06}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};

    /// Options carry profile fields and the default port; values map across scalar families, text protocol bytes coerce, and hostile cells are bounded.
    #[test]
    fn mysql_options_and_value_mapping_are_faithful_and_bounded() {
        // --- phase: opts_carry_profile_fields ---
        {
            let mut p = ConnectionProfile::new("shop", Engine::MySql);
            p.host = "mysql.example".into();
            p.port = 3307;
            p.database = "shop".into();
            p.user = "app".into();
            p.password = "secret".into();
            let opts: Opts = mysql_opts(&p).into();
            assert_eq!(opts.get_ip_or_hostname().as_ref(), "mysql.example");
            assert_eq!(opts.get_tcp_port(), 3307);
            assert_eq!(opts.get_user(), Some("app"));
            assert_eq!(opts.get_db_name(), Some("shop"));
        }

        // --- phase: opts_default_port_when_zero ---
        {
            let mut p = ConnectionProfile::new("x", Engine::MySql);
            p.port = 0;
            let opts: Opts = mysql_opts(&p).into();
            assert_eq!(opts.get_tcp_port(), 3306);
        }

        // --- phase: value_mapping_covers_scalar_families ---
        {
            assert_eq!(mysql_value_to_cell(Value::NULL), Cell::Null);
            assert_eq!(mysql_value_to_cell(Value::Int(-7)), Cell::Integer(-7));
            assert_eq!(mysql_value_to_cell(Value::UInt(9)), Cell::Integer(9));
            assert_eq!(mysql_value_to_cell(Value::Double(1.5)), Cell::Real(1.5));
            assert_eq!(
                mysql_value_to_cell(Value::Bytes(b"hi".to_vec())),
                Cell::Text("hi".into())
            );
            assert_eq!(
                mysql_value_to_cell(Value::Date(2026, 7, 28, 0, 0, 0, 0)),
                Cell::Text("2026-07-28".into())
            );
            assert_eq!(
                mysql_value_to_cell(Value::Date(2026, 7, 28, 9, 30, 0, 0)),
                Cell::Text("2026-07-28 09:30:00".into())
            );
        }

        // --- phase: text_protocol_bytes_coerce_to_typed_numerics ---
        {
            // The text protocol hands back integer/float columns as raw bytes; the
            // column type recovers a typed numeric cell so the grid right-aligns.
            assert_eq!(
                cell_from_value(
                    Value::Bytes(b"42".to_vec()),
                    Some(ColumnType::MYSQL_TYPE_LONG)
                ),
                Cell::Integer(42)
            );
            assert_eq!(
                cell_from_value(
                    Value::Bytes(b"1.5".to_vec()),
                    Some(ColumnType::MYSQL_TYPE_DOUBLE)
                ),
                Cell::Real(1.5)
            );
            // A text column stays text even when the bytes look numeric.
            assert_eq!(
                cell_from_value(
                    Value::Bytes(b"42".to_vec()),
                    Some(ColumnType::MYSQL_TYPE_VAR_STRING)
                ),
                Cell::Text("42".into())
            );
            // NULL and already-typed binary-protocol values pass straight through.
            assert_eq!(
                cell_from_value(Value::NULL, Some(ColumnType::MYSQL_TYPE_LONG)),
                Cell::Null
            );
            assert_eq!(
                cell_from_value(Value::Int(7), Some(ColumnType::MYSQL_TYPE_LONG)),
                Cell::Integer(7)
            );
        }

        // --- phase: hostile_text_protocol_cell_is_retained_at_a_bounded_size ---
        {
            let Cell::Text(value) = cell_from_value(
                Value::Bytes(vec![b'x'; Cell::MAX_RETAINED_TEXT_BYTES + 99]),
                Some(ColumnType::MYSQL_TYPE_VAR_STRING),
            ) else {
                panic!("text cell expected")
            };
            assert!(value.len() <= Cell::MAX_RETAINED_TEXT_BYTES + 32);
            assert!(value.ends_with("… [truncated]"));
        }
    }
}
