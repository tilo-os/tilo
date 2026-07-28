//! MySQL / MariaDB execution engine: opens a real TCP connection through the
//! sync `mysql` crate, introspects `information_schema` and runs arbitrary SQL
//! returning a [`QueryResult`]. Mirrors [`crate::engine::SqliteEngine`] and
//! [`crate::engine_pg::PostgresEngine`] so the session can hold any engine
//! behind one enum.

use std::time::{Duration, Instant};

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Value};

use crate::model::{
    Cell, ColumnInfo, DatabaseSchema, ForeignKey, IndexInfo, QueryResult, RelationKind, SchemaInfo,
    TableInfo, TypeClass,
};

/// A live MySQL connection. Owns the connection for the session lifetime.
pub struct MySqlEngine {
    conn: Conn,
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
        .tcp_port(if profile.port == 0 { 3306 } else { profile.port })
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
    pub fn connect(profile: &crate::model::ConnectionProfile) -> Result<Self, String> {
        let opts: Opts = mysql_opts(profile).into();
        let conn = Conn::new(opts).map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Introspect the connection's current database into the shared
    /// [`DatabaseSchema`] shape. MySQL's schema namespace is the database, so
    /// this yields a single [`SchemaInfo`] named after it.
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
                        class: TypeClass::classify(&ctype),
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
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult::default());
        }
        let start = Instant::now();

        let mut result = self.conn.query_iter(trimmed).map_err(|e| e.to_string())?;
        let columns: Vec<String> = result
            .columns()
            .as_ref()
            .iter()
            .map(|c| c.name_str().to_string())
            .collect();

        if columns.is_empty() {
            let affected = result.affected_rows() as i64;
            drop(result);
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                elapsed_ms: elapsed_ms(start),
                rows_affected: Some(affected),
                is_select: false,
            });
        }

        let mut rows_out: Vec<Vec<Cell>> = Vec::new();
        for row_res in result.by_ref() {
            let row = row_res.map_err(|e| e.to_string())?;
            let values: Vec<Value> = row.unwrap();
            rows_out.push(values.into_iter().map(mysql_value_to_cell).collect());
        }
        drop(result);

        Ok(QueryResult {
            columns,
            rows: rows_out,
            elapsed_ms: elapsed_ms(start),
            rows_affected: None,
            is_select: true,
        })
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Map a driver [`Value`] to a [`Cell`]. Integers/floats keep their type;
/// byte strings become text when valid UTF-8, otherwise a blob length;
/// temporal values render as their canonical string form.
pub(crate) fn mysql_value_to_cell(v: Value) -> Cell {
    match v {
        Value::NULL => Cell::Null,
        Value::Int(i) => Cell::Integer(i),
        Value::UInt(u) => {
            if u <= i64::MAX as u64 {
                Cell::Integer(u as i64)
            } else {
                Cell::Text(u.to_string())
            }
        }
        Value::Float(f) => Cell::Real(f as f64),
        Value::Double(d) => Cell::Real(d),
        Value::Bytes(b) => match String::from_utf8(b) {
            Ok(s) => Cell::Text(s),
            Err(e) => Cell::Blob(e.into_bytes().len()),
        },
        Value::Date(y, mo, d, h, mi, s, us) => {
            if h == 0 && mi == 0 && s == 0 && us == 0 {
                Cell::Text(format!("{y:04}-{mo:02}-{d:02}"))
            } else if us == 0 {
                Cell::Text(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
            } else {
                Cell::Text(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}"))
            }
        }
        Value::Time(neg, days, h, mi, s, us) => {
            let sign = if neg { "-" } else { "" };
            let hours = days * 24 + h as u32;
            if us == 0 {
                Cell::Text(format!("{sign}{hours:02}:{mi:02}:{s:02}"))
            } else {
                Cell::Text(format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};

    #[test]
    fn opts_carry_profile_fields() {
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

    #[test]
    fn opts_default_port_when_zero() {
        let mut p = ConnectionProfile::new("x", Engine::MySql);
        p.port = 0;
        let opts: Opts = mysql_opts(&p).into();
        assert_eq!(opts.get_tcp_port(), 3306);
    }

    #[test]
    fn value_mapping_covers_scalar_families() {
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
}
