//! `PostgreSQL` execution engine: opens a real TCP connection through the sync
//! `postgres` crate, introspects the catalog (`information_schema` +
//! `pg_catalog`) and runs arbitrary SQL returning a [`QueryResult`]. Mirrors
//! the shape of [`crate::engine::SqliteEngine`] so the session layer can hold
//! any engine behind one enum.
//!
//! Row-returning statements stream through `PostgreSQL`'s prepared cursor and
//! stop at the app result limit. Common scalar values are decoded directly;
//! unsupported binary types remain explicitly labelled rather than being
//! confused with a database NULL.

use ui::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use postgres::fallible_iterator::FallibleIterator as PgFallibleIterator;
use postgres::types::{FromSql, Kind, Type};
use postgres::{Client, Config, NoTls};

use crate::engine::{MAX_QUERY_ROWS, can_retain_result_row, row_retained_bytes};
use crate::model::{
    Cell, ColumnInfo, DatabaseSchema, ForeignKey, IndexInfo, QueryResult, RelationKind, SchemaInfo,
    TableInfo,
};

/// A live `PostgreSQL` connection. Owns the client for the session lifetime.
pub struct PostgresEngine {
    client: Client,
}

struct PgNumeric(String);

/// A rendered one-dimensional `PostgreSQL` array. `PostgreSQL`'s binary array
/// representation carries dimensions explicitly, allowing us to reject a
/// multidimensional value honestly instead of flattening or misrepresenting
/// it. Individual NULL elements remain the literal `NULL` in the display.
struct PgArray(String);

const MAX_PG_ARRAY_ELEMENTS: usize = 100_000;

impl FromSql<'_> for PgArray {
    #[allow(
        clippy::too_many_lines,
        reason = "one binary format decoder keeps header validation, bounded rendering, and element offset advancement auditable together"
    )]
    fn from_sql(ty: &Type, raw: &[u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let read_i32 = |offset: usize| -> Result<i32, Box<dyn std::error::Error + Sync + Send>> {
            let end = offset
                .checked_add(4)
                .ok_or("PostgreSQL array header offset overflow")?;
            let bytes: [u8; 4] = raw
                .get(offset..end)
                .ok_or("truncated PostgreSQL array header")?
                .try_into()
                .map_err(|_| "invalid PostgreSQL array header")?;
            Ok(i32::from_be_bytes(bytes))
        };
        let dimensions = read_i32(0)?;
        if dimensions < 0 {
            return Err("invalid PostgreSQL array dimensions".into());
        }
        let dimensions =
            usize::try_from(dimensions).map_err(|_| "invalid PostgreSQL array dimensions")?;
        let header_len = 12usize
            .checked_add(
                dimensions
                    .checked_mul(8)
                    .ok_or("PostgreSQL array dimension overflow")?,
            )
            .ok_or("PostgreSQL array header overflow")?;
        if raw.len() < header_len {
            return Err("truncated PostgreSQL array header".into());
        }
        let Kind::Array(element) = ty.kind() else {
            return Err("expected PostgreSQL array type".into());
        };
        let element_oid =
            u32::try_from(read_i32(8)?).map_err(|_| "invalid PostgreSQL array element OID")?;
        if element_oid != element.oid() {
            return Err("PostgreSQL array element OID does not match its declared type".into());
        }
        if dimensions == 0 {
            if raw.len() != 12 {
                return Err("invalid PostgreSQL empty array length".into());
            }
            return Ok(Self("[]".into()));
        }
        if dimensions != 1 {
            return Ok(Self(format!(
                "<{}: {dimensions}-dimensional array unsupported>",
                pg_array_type_label(ty)
            )));
        }
        let length =
            usize::try_from(read_i32(12)?).map_err(|_| "negative PostgreSQL array length")?;
        if length > MAX_PG_ARRAY_ELEMENTS {
            return Ok(Self(format!(
                "<{}: {length} elements exceeds display limit of {MAX_PG_ARRAY_ELEMENTS}>",
                pg_array_type_label(ty)
            )));
        }
        if !pg_array_element_supported(element) {
            return Ok(Self(format!(
                "<{}: unsupported array element type {}>",
                pg_array_type_label(ty),
                element.name()
            )));
        }
        let mut offset = header_len;
        let mut rendered = String::from("[");
        let suffix = "]… [truncated]";
        let content_limit = Cell::MAX_RETAINED_TEXT_BYTES.saturating_sub(suffix.len());
        let mut truncated = false;
        for item_index in 0..length {
            if raw.len().saturating_sub(offset) < 4 {
                return Err("truncated PostgreSQL array element length".into());
            }
            let value_len = read_i32(offset)?;
            offset += 4;
            let value = if value_len == -1 {
                "NULL".to_string()
            } else {
                let value_len = usize::try_from(value_len)
                    .map_err(|_| "negative PostgreSQL array element length")?;
                let end = offset
                    .checked_add(value_len)
                    .ok_or("PostgreSQL array element length overflow")?;
                let bytes = raw
                    .get(offset..end)
                    .ok_or("truncated PostgreSQL array element")?;
                offset = end;
                pg_array_element(element, bytes)?
            };
            if !truncated {
                let prefix = if item_index == 0 { "" } else { ", " };
                for character in prefix.chars().chain(value.chars()) {
                    if rendered.len().saturating_add(character.len_utf8()) > content_limit {
                        truncated = true;
                        break;
                    }
                    rendered.push(character);
                }
            }
        }
        if offset != raw.len() {
            return Err("invalid trailing PostgreSQL array bytes".into());
        }
        if truncated {
            rendered.push_str(suffix);
        } else {
            rendered.push(']');
        }
        Ok(Self(rendered))
    }

    fn accepts(ty: &Type) -> bool {
        matches!(ty.kind(), Kind::Array(_))
    }
}

fn pg_array_element(
    ty: &Type,
    raw: &[u8],
) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    let fixed = |length| -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
        (raw.len() == length)
            .then_some(())
            .ok_or_else(|| format!("invalid {} array element length", ty.name()).into())
    };
    let quoted = |value: String| serde_json::to_string(&value).map_err(Into::into);
    match *ty {
        Type::INT2 => {
            fixed(2)?;
            let bytes: [u8; 2] = raw
                .try_into()
                .map_err(|_| "invalid PostgreSQL int2 array element")?;
            Ok(i16::from_be_bytes(bytes).to_string())
        }
        Type::INT4 => {
            fixed(4)?;
            let bytes: [u8; 4] = raw
                .try_into()
                .map_err(|_| "invalid PostgreSQL int4 array element")?;
            Ok(i32::from_be_bytes(bytes).to_string())
        }
        Type::INT8 => {
            fixed(8)?;
            let bytes: [u8; 8] = raw
                .try_into()
                .map_err(|_| "invalid PostgreSQL int8 array element")?;
            Ok(i64::from_be_bytes(bytes).to_string())
        }
        Type::FLOAT4 => {
            fixed(4)?;
            let bytes: [u8; 4] = raw
                .try_into()
                .map_err(|_| "invalid PostgreSQL float4 array element")?;
            Ok(f32::from_bits(u32::from_be_bytes(bytes)).to_string())
        }
        Type::FLOAT8 => {
            fixed(8)?;
            let bytes: [u8; 8] = raw
                .try_into()
                .map_err(|_| "invalid PostgreSQL float8 array element")?;
            Ok(f64::from_bits(u64::from_be_bytes(bytes)).to_string())
        }
        Type::BOOL => match raw {
            [0] => Ok("false".into()),
            [1] => Ok("true".into()),
            _ => Err("invalid PostgreSQL bool array element".into()),
        },
        Type::NUMERIC => Ok(PgNumeric::from_sql(ty, raw)?.0),
        Type::UUID => quoted(uuid::Uuid::from_slice(raw)?.to_string()),
        Type::DATE => quoted(NaiveDate::from_sql(ty, raw)?.to_string()),
        Type::TIME => quoted(NaiveTime::from_sql(ty, raw)?.to_string()),
        Type::TIMESTAMP => quoted(NaiveDateTime::from_sql(ty, raw)?.to_string()),
        Type::TIMESTAMPTZ => quoted(DateTime::<Utc>::from_sql(ty, raw)?.to_rfc3339()),
        Type::JSON | Type::JSONB => Ok(serde_json::Value::from_sql(ty, raw)?.to_string()),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            quoted(std::str::from_utf8(raw)?.to_string())
        }
        _ => Err(format!("unsupported PostgreSQL array element type {}", ty.name()).into()),
    }
}

fn pg_array_element_supported(ty: &Type) -> bool {
    matches!(
        *ty,
        Type::INT2
            | Type::INT4
            | Type::INT8
            | Type::FLOAT4
            | Type::FLOAT8
            | Type::BOOL
            | Type::NUMERIC
            | Type::UUID
            | Type::DATE
            | Type::TIME
            | Type::TIMESTAMP
            | Type::TIMESTAMPTZ
            | Type::JSON
            | Type::JSONB
            | Type::TEXT
            | Type::VARCHAR
            | Type::BPCHAR
            | Type::NAME
    )
}

fn pg_array_type_label(ty: &Type) -> String {
    ty.name()
        .strip_prefix('_')
        .map_or_else(|| ty.name().to_string(), |element| format!("{element}[]"))
}

#[allow(
    clippy::indexing_slicing,
    clippy::format_push_string,
    reason = "length is validated before fixed-width PostgreSQL numeric decoding"
)]
impl FromSql<'_> for PgNumeric {
    fn from_sql(_: &Type, raw: &[u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        if raw.len() < 8 {
            return Err("invalid PostgreSQL numeric".into());
        }
        let read = |at| i16::from_be_bytes([raw[at], raw[at + 1]]);
        let ndigits = usize::try_from(read(0)).map_err(|_| "negative numeric digit count")?;
        if raw.len() != 8 + ndigits * 2 {
            return Err("invalid PostgreSQL numeric length".into());
        }
        let weight = read(2);
        let sign = u16::from_be_bytes([raw[4], raw[5]]);
        let scale = usize::from(u16::from_be_bytes([raw[6], raw[7]]));
        match sign {
            0x0000 | 0x4000 => {}
            0xC000 => return Ok(Self("NaN".into())),
            0xD000 => return Ok(Self("Infinity".into())),
            0xF000 => return Ok(Self("-Infinity".into())),
            _ => return Err("invalid PostgreSQL numeric sign".into()),
        }
        let mut groups = Vec::with_capacity(ndigits);
        for i in 0..ndigits {
            let digit = u16::from_be_bytes([raw[8 + i * 2], raw[9 + i * 2]]);
            if digit >= 10_000 {
                return Err("invalid PostgreSQL numeric digit".into());
            }
            groups.push(digit);
        }
        if groups.iter().all(|digit| *digit == 0) {
            return Ok(Self(if scale == 0 {
                "0".into()
            } else {
                format!("0.{:0<width$}", "", width = scale)
            }));
        }
        let mut integer = String::new();
        if weight < 0 {
            integer.push('0');
        } else {
            for exponent in (0..=weight).rev() {
                let index = usize::try_from(i32::from(weight - exponent)).unwrap_or(0);
                let digit = groups.get(index).copied().unwrap_or(0);
                if exponent == weight {
                    integer.push_str(&digit.to_string());
                } else {
                    integer.push_str(&format!("{digit:04}"));
                }
            }
        }
        let mut fraction = String::new();
        let mut fractional_group = 1i16;
        while fraction.len() < scale {
            let index = isize::from(weight) + isize::from(fractional_group);
            let digit = if index >= 0 {
                groups
                    .get(usize::try_from(index).unwrap_or(0))
                    .copied()
                    .unwrap_or(0)
            } else {
                0
            };
            fraction.push_str(&format!("{digit:04}"));
            fractional_group += 1;
        }
        fraction.truncate(scale);
        let sign_prefix = if sign == 0x4000 { "-" } else { "" };
        Ok(Self(if scale == 0 {
            format!("{sign_prefix}{integer}")
        } else {
            format!("{sign_prefix}{integer}.{fraction}")
        }))
    }
    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

fn pg_stream_cell(row: &postgres::Row, index: usize) -> Result<Cell, String> {
    let Some(column) = row.columns().get(index) else {
        return Err("PostgreSQL returned a row with a missing column".into());
    };
    let ty = column.type_();
    if matches!(ty.kind(), Kind::Array(_)) {
        return row
            .try_get::<_, Option<PgArray>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.0)));
    }
    match *ty {
        Type::INT2 => row
            .try_get::<_, Option<i16>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::Integer(i64::from(v)))),
        Type::INT4 => row
            .try_get::<_, Option<i32>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::Integer(i64::from(v)))),
        Type::INT8 => row
            .try_get::<_, Option<i64>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, Cell::Integer)),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::Real(f64::from(v)))),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, Cell::Real)),
        Type::BOOL => row
            .try_get::<_, Option<bool>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::BYTEA => row
            .try_get::<_, Option<Vec<u8>>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::Blob(v.len()))),
        Type::NUMERIC => row
            .try_get::<_, Option<PgNumeric>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.0))),
        Type::UUID => row
            .try_get::<_, Option<uuid::Uuid>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::DATE => row
            .try_get::<_, Option<NaiveDate>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::TIME => row
            .try_get::<_, Option<NaiveTime>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::TIMESTAMP => row
            .try_get::<_, Option<NaiveDateTime>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::TIMESTAMPTZ => row
            .try_get::<_, Option<DateTime<Utc>>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_rfc3339()))),
        Type::JSON | Type::JSONB => row
            .try_get::<_, Option<serde_json::Value>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, |v| Cell::bounded_text(v.to_string()))),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => row
            .try_get::<_, Option<String>>(index)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Cell::Null, Cell::bounded_text)),
        _ => Ok(Cell::Text(format!("<{}: binary value>", ty.name()))),
    }
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
    cfg.application_name("tilo-db-client");
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

    pub(crate) fn cancellation_handle(&self) -> crate::engine::QueryCancellation {
        crate::engine::QueryCancellation::Postgres(self.client.cancel_token())
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
    /// names + whether the statement returns rows, then streams bounded rows
    /// through the prepared cursor. Multiple statements are rejected.
    pub fn run(&mut self, sql: &str) -> Result<QueryResult, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult::default());
        }
        let start = ui::time::now();

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
                    truncation: None,
                })
            }
            Ok(stmt) => {
                let columns: Vec<String> = stmt
                    .columns()
                    .iter()
                    .map(|c| c.name().to_string())
                    .collect();
                let mut cursor = self
                    .client
                    .query_raw(&stmt, std::iter::empty::<&str>())
                    .map_err(|e| e.to_string())?;
                let mut rows = Vec::new();
                let mut truncation: Option<crate::model::Truncation> = None;
                let mut retained_bytes = 0usize;
                let mut retained_cells = 0usize;
                while let Some(row) = cursor.next().map_err(|e| e.to_string())? {
                    if rows.len() == MAX_QUERY_ROWS {
                        truncation = Some(crate::model::Truncation::RowLimit);
                        break;
                    }
                    let cells = (0..row.len())
                        .map(|i| pg_stream_cell(&row, i))
                        .collect::<Result<Vec<_>, _>>()?;
                    if !can_retain_result_row(retained_bytes, retained_cells, &cells) {
                        truncation = Some(crate::model::Truncation::Payload);
                        break;
                    }
                    retained_bytes += row_retained_bytes(&cells);
                    retained_cells += cells.len();
                    rows.push(cells);
                }
                Ok(QueryResult {
                    columns,
                    rows,
                    elapsed_ms: elapsed_ms(start),
                    rows_affected: None,
                    is_select: true,
                    truncation,
                })
            }
            Err(error) => Err(format!(
                "Only one SQL statement at a time is supported: {error}"
            )),
        }
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};

    fn numeric(weight: i16, sign: u16, scale: u16, digits: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(digits.len() as i16).to_be_bytes());
        out.extend_from_slice(&weight.to_be_bytes());
        out.extend_from_slice(&sign.to_be_bytes());
        out.extend_from_slice(&scale.to_be_bytes());
        for digit in digits {
            out.extend_from_slice(&digit.to_be_bytes());
        }
        out
    }

    /// Binary numerics preserve exponents, scale and specials and binary arrays preserve values, nulls and dimensions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn pg_binary_values_preserve_numeric_and_array_semantics() {
        // --- phase: binary_numeric_preserves_exponents_scale_and_specials ---
        {
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0, 2, &[1, 2300]))
                    .unwrap()
                    .0,
                "1.23"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(-1, 0, 4, &[12]))
                    .unwrap()
                    .0,
                "0.0012"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(2, 0, 0, &[1, 2345]))
                    .unwrap()
                    .0,
                "123450000"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0x4000, 4, &[42, 5000]))
                    .unwrap()
                    .0,
                "-42.5000"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0, 3, &[0]))
                    .unwrap()
                    .0,
                "0.000"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0xC000, 0, &[]))
                    .unwrap()
                    .0,
                "NaN"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0xD000, 0, &[]))
                    .unwrap()
                    .0,
                "Infinity"
            );
            assert_eq!(
                PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0xF000, 0, &[]))
                    .unwrap()
                    .0,
                "-Infinity"
            );
            assert!(PgNumeric::from_sql(&Type::NUMERIC, &[0; 7]).is_err());
            let mut short_numeric = numeric(0, 0, 0, &[]);
            short_numeric[..2].copy_from_slice(&1i16.to_be_bytes());
            assert!(PgNumeric::from_sql(&Type::NUMERIC, &short_numeric).is_err());
            assert!(PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0, 0, &[10_000])).is_err());
            assert!(PgNumeric::from_sql(&Type::NUMERIC, &numeric(0, 0x1234, 0, &[])).is_err());
        }

        // --- phase: binary_arrays_preserve_values_nulls_and_dimension_honesty ---
        {
            let ints = array_1d(
                23,
                &[
                    Some(&1i32.to_be_bytes()),
                    None,
                    Some(&(-2i32).to_be_bytes()),
                ],
            );
            assert_eq!(
                PgArray::from_sql(&Type::INT4_ARRAY, &ints).unwrap().0,
                "[1, NULL, -2]"
            );
            let text = array_1d(25, &[Some(b"alpha"), None, Some(b"two words")]);
            assert_eq!(
                PgArray::from_sql(&Type::TEXT_ARRAY, &text).unwrap().0,
                r#"["alpha", NULL, "two words"]"#
            );
            let mut empty = Vec::new();
            empty.extend_from_slice(&0i32.to_be_bytes());
            empty.extend_from_slice(&0i32.to_be_bytes());
            empty.extend_from_slice(&23u32.to_be_bytes());
            assert_eq!(
                PgArray::from_sql(&Type::INT4_ARRAY, &empty).unwrap().0,
                "[]"
            );
            let mut multidimensional = Vec::new();
            multidimensional.extend_from_slice(&2i32.to_be_bytes());
            multidimensional.extend_from_slice(&0i32.to_be_bytes());
            multidimensional.extend_from_slice(&23u32.to_be_bytes());
            multidimensional.extend_from_slice(&1i32.to_be_bytes());
            multidimensional.extend_from_slice(&1i32.to_be_bytes());
            multidimensional.extend_from_slice(&1i32.to_be_bytes());
            multidimensional.extend_from_slice(&1i32.to_be_bytes());
            assert_eq!(
                PgArray::from_sql(&Type::INT4_ARRAY, &multidimensional)
                    .unwrap()
                    .0,
                "<int4[]: 2-dimensional array unsupported>"
            );
            let mut huge = Vec::new();
            huge.extend_from_slice(&1i32.to_be_bytes());
            huge.extend_from_slice(&0i32.to_be_bytes());
            huge.extend_from_slice(&23u32.to_be_bytes());
            huge.extend_from_slice(&100_001i32.to_be_bytes());
            huge.extend_from_slice(&1i32.to_be_bytes());
            assert_eq!(
                PgArray::from_sql(&Type::INT4_ARRAY, &huge).unwrap().0,
                "<int4[]: 100001 elements exceeds display limit of 100000>"
            );
            assert!(PgArray::from_sql(&Type::INT4_ARRAY, &[0; 11]).is_err());
            let wrong_oid = array_1d(25, &[Some(&1i32.to_be_bytes())]);
            assert!(PgArray::from_sql(&Type::INT4_ARRAY, &wrong_oid).is_err());
        }
    }

    fn array_1d(element_oid: u32, values: &[Option<&[u8]>]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&1i32.to_be_bytes());
        raw.extend_from_slice(&i32::from(values.iter().any(Option::is_none)).to_be_bytes());
        raw.extend_from_slice(&element_oid.to_be_bytes());
        raw.extend_from_slice(&(values.len() as i32).to_be_bytes());
        raw.extend_from_slice(&1i32.to_be_bytes());
        for value in values {
            match value {
                Some(value) => {
                    raw.extend_from_slice(&(value.len() as i32).to_be_bytes());
                    raw.extend_from_slice(value);
                }
                None => raw.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        raw
    }

    /// The config carries profile fields and defaults an empty port to 5432.
    #[test]
    fn pg_config_carries_profile_fields_and_default_port() {
        // --- phase: config_carries_profile_fields ---
        {
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
            assert!(
                cfg.get_hosts()
                    .iter()
                    .any(|h| matches!(h, postgres::config::Host::Tcp(t) if t == "db.example")),
                "host not set on config"
            );
        }

        // --- phase: config_defaults_empty_port_to_5432 ---
        {
            let mut p = ConnectionProfile::new("x", Engine::Postgres);
            p.port = 0;
            p.host = String::new();
            let cfg = pg_config(&p);
            assert_eq!(cfg.get_ports(), &[5432]);
        }
    }
}
