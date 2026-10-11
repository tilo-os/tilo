//! Pure data types for the database client: connection profiles, the
//! introspected schema tree, and query results. No IO and no rendering —
//! the engine layer fills these in, the widgets read them.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model};
use uuid::Uuid;

/// Which database engine a connection targets. `SQLite` is fully local and
/// always available; Postgres and `MySQL` are network engines whose live
/// drivers are gated behind a sandbox grant (see [`ConnectionProfile`]).
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Sqlite,
    #[default]
    Postgres,
    #[model(rename = "mysql")]
    MySql,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::Sqlite => "SQLite",
            Engine::Postgres => "PostgreSQL",
            Engine::MySql => "MySQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Engine::Sqlite => 0,
            Engine::Postgres => 5432,
            Engine::MySql => 3306,
        }
    }

    pub fn all() -> [Engine; 3] {
        [Engine::Sqlite, Engine::Postgres, Engine::MySql]
    }

    /// Stable string key an engine is named by where it is typed: a verb's
    /// parameter, the record. Kept distinct from [`label`](Self::label) so
    /// the key never shifts with a UI rename.
    pub fn as_key(self) -> &'static str {
        match self {
            Engine::Sqlite => "sqlite",
            Engine::Postgres => "postgres",
            Engine::MySql => "mysql",
        }
    }

    /// Parse an engine back from its [`as_key`](Self::as_key) form, defaulting
    /// to `PostgreSQL` for an unknown key so a forward-compatible document still
    /// renders something sensible.
    pub fn from_key(key: &str) -> Engine {
        match key {
            "sqlite" => Engine::Sqlite,
            "mysql" => Engine::MySql,
            _ => Engine::Postgres,
        }
    }
}

/// A saved connection. Network engines carry host/port/user; `SQLite` carries
/// a file path. `connected` reflects whether a live handle is currently open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    /// Network engines: TCP host. `SQLite`: ignored.
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Network engines: password used to authenticate. Held in memory only —
    /// `#[serde(skip)]` keeps it out of the on-disk profile so plaintext
    /// secrets are never persisted; the user re-enters it per session.
    #[serde(skip, default)]
    pub password: String,
    /// `SQLite` file path (or `:memory:` for the bundled sample).
    pub file_path: String,
    /// Whether a live handle is currently open for this profile.
    #[serde(default)]
    pub connected: bool,
    /// True for the read-only bundled sample database that ships with the
    /// app so the schema browser and query runner work offline.
    #[serde(default)]
    pub is_sample: bool,
}

impl ConnectionProfile {
    pub fn sqlite_sample() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "Chinook sample".to_string(),
            engine: Engine::Sqlite,
            host: String::new(),
            port: 0,
            database: "main".to_string(),
            file_path: ":memory:".to_string(),
            user: String::new(),
            password: String::new(),
            connected: true,
            is_sample: true,
        }
    }

    pub fn new(name: &str, engine: Engine) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            engine,
            host: "localhost".to_string(),
            port: engine.default_port(),
            database: String::new(),
            user: String::new(),
            password: String::new(),
            file_path: String::new(),
            connected: false,
            is_sample: false,
        }
    }

    /// Human-readable target ("host:port/db" or a file path) for the
    /// connection list subtitle.
    pub fn subtitle(&self) -> String {
        match self.engine {
            Engine::Sqlite => {
                if self.file_path == ":memory:" {
                    "in-memory".to_string()
                } else if self.file_path.is_empty() {
                    "no file".to_string()
                } else {
                    self.file_path.clone()
                }
            }
            _ => format!("{}:{}/{}", self.host, self.port, self.database),
        }
    }

    /// Where the connection goes, as the sidebar's second line: `host:port`
    /// for a network engine, the file's name for `SQLite`, nothing for the
    /// bundled sample.
    pub fn address(&self) -> String {
        match self.engine {
            Engine::Sqlite if self.is_sample || self.file_path == ":memory:" => String::new(),
            Engine::Sqlite => std::path::Path::new(&self.file_path)
                .file_name()
                .map_or_else(
                    || self.file_path.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
            Engine::Postgres | Engine::MySql => format!("{}:{}", self.host, self.port),
        }
    }

    /// Network engines need a sandbox grant before a live handle can open.
    pub fn is_remote(&self) -> bool {
        !matches!(self.engine, Engine::Sqlite)
    }
}

/// SQL data-type family, used to pick a type icon + colour in the schema
/// tree and to right-align numeric result columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeClass {
    Integer,
    Real,
    Text,
    Blob,
    Boolean,
    DateTime,
    Numeric,
    Unknown,
}

impl TypeClass {
    /// Classify a raw SQL type declaration the way `SQLite`'s loose typing
    /// does — substring match against the declared type string.
    pub fn classify(decl: &str) -> TypeClass {
        let d = decl.to_ascii_uppercase();
        if d.contains("INT") {
            TypeClass::Integer
        } else if d.contains("BOOL") {
            TypeClass::Boolean
        } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
            TypeClass::Text
        } else if d.contains("BLOB") || d.is_empty() {
            TypeClass::Blob
        } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
            TypeClass::Real
        } else if d.contains("DATE") || d.contains("TIME") {
            TypeClass::DateTime
        } else if d.contains("NUMERIC") || d.contains("DECIMAL") || d.contains("MONEY") {
            TypeClass::Numeric
        } else {
            TypeClass::Unknown
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            TypeClass::Integer | TypeClass::Real | TypeClass::Numeric | TypeClass::Boolean
        )
    }
}

/// A single column of a table, as introspected from the engine.
#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub class: TypeClass,
    pub not_null: bool,
    pub primary_key: bool,
    pub default: Option<String>,
}

/// A foreign-key constraint on a table.
#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

/// A secondary index on a table.
#[derive(Clone, Debug)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// What kind of relation a schema entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
}

/// A table or view plus its full structure.
#[derive(Clone, Debug)]
pub struct TableInfo {
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    pub indexes: Vec<IndexInfo>,
    /// Estimated row count (a `COUNT(*)` for tables, `None` for views).
    pub row_count: Option<i64>,
}

/// A relation's unambiguous catalog address. Table names are only unique
/// inside a schema, so live browse operations must never use a bare name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelationRef {
    pub schema: String,
    pub name: String,
}

impl TableInfo {
    pub fn primary_keys(&self) -> Vec<&ColumnInfo> {
        self.columns.iter().filter(|c| c.primary_key).collect()
    }
}

/// A logical schema namespace. `SQLite` has a single implicit "main" schema;
/// Postgres/MySQL expose several. The tree renders database → schema →
/// tables → columns.
#[derive(Clone, Debug)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: Vec<TableInfo>,
}

/// The fully-introspected schema for one connection.
#[derive(Clone, Debug, Default)]
pub struct DatabaseSchema {
    pub schemas: Vec<SchemaInfo>,
}

impl DatabaseSchema {
    pub fn table_count(&self) -> usize {
        self.schemas.iter().map(|s| s.tables.len()).sum()
    }

    pub fn find_table(&self, name: &str) -> Option<&TableInfo> {
        self.schemas
            .iter()
            .flat_map(|s| s.tables.iter())
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn find_relation(&self, schema: &str, name: &str) -> Option<&TableInfo> {
        self.schemas
            .iter()
            .find(|candidate| candidate.name == schema)?
            .tables
            .iter()
            .find(|table| table.name == name)
    }

    pub fn relation_named(&self, name: &str) -> Option<RelationRef> {
        self.schemas.iter().find_map(|schema| {
            schema
                .tables
                .iter()
                .find(|table| table.name == name)
                .map(|table| RelationRef {
                    schema: schema.name.clone(),
                    name: table.name.clone(),
                })
        })
    }
}

/// A single cell value in a result set. Kept as a small enum so NULL can be
/// styled distinctly and numbers can be right-aligned.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(usize),
}

impl Cell {
    pub const MAX_RETAINED_TEXT_BYTES: usize = 16 * 1024;
    #[allow(
        clippy::string_slice,
        reason = "end is moved back to a validated UTF-8 boundary"
    )]
    pub fn bounded_text(value: String) -> Self {
        if value.len() <= Self::MAX_RETAINED_TEXT_BYTES {
            return Self::Text(value);
        }
        let mut end = Self::MAX_RETAINED_TEXT_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self::Text(format!("{}… [truncated]", &value[..end]))
    }

    /// Approximate owned payload retained by this cell. Blob payloads are not
    /// kept, only their size metadata, so they consume no result-data budget.
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::Text(value) => value.len(),
            Self::Integer(_) | Self::Real(_) => std::mem::size_of::<i64>(),
            Self::Null | Self::Blob(_) => 0,
        }
    }
    /// Display string for the grid. NULL renders the literal token; callers
    /// style it dim+italic.
    pub fn display(&self) -> String {
        match self {
            Cell::Null => "NULL".to_string(),
            Cell::Integer(i) => i.to_string(),
            Cell::Real(r) => format_real(*r),
            Cell::Text(s) => s.clone(),
            Cell::Blob(n) => format!("Binary, {n} bytes"),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Cell::Integer(_) | Cell::Real(_))
    }
}

#[allow(
    clippy::float_cmp,
    reason = "trunc() is an exact, non-accumulating operation on r itself, not an independently \
              computed value, so this is testing whether r already is a whole number, not \
              tolerating rounding error"
)]
fn format_real(r: f64) -> String {
    if r == r.trunc() && r.abs() < 1e15 {
        format!("{r:.1}")
    } else {
        let s = format!("{r:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }
}

/// The result of running a SQL statement.
#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// Wall-clock time the query took.
    pub elapsed_ms: f64,
    /// For non-SELECT statements: rows affected.
    pub rows_affected: Option<i64>,
    /// True when the statement returned a row set (SELECT/PRAGMA), false for
    /// DML/DDL.
    pub is_select: bool,
    /// True when collection stopped at the app's bounded result limit. The
    /// grid can stay responsive without pretending the server returned only
    /// these rows.
    pub truncated: bool,
}

impl QueryResult {
    pub fn message(&self) -> String {
        if self.is_select {
            format!(
                "{} rows{} in {:.1} ms",
                self.rows.len(),
                if self.truncated { "+" } else { "" },
                self.elapsed_ms
            )
        } else {
            let n = self.rows_affected.unwrap_or(0);
            format!("{n} rows affected in {:.1} ms", self.elapsed_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Columns classify integer families, reals display without trailing zeros, hostile text is refused, and profiles report local or network engines.
    #[test]
    fn cell_and_profile_classification() {
        // --- phase: classify_picks_integer_for_int_family ---
        {
            assert_eq!(TypeClass::classify("INTEGER"), TypeClass::Integer);
            assert_eq!(TypeClass::classify("BIGINT"), TypeClass::Integer);
            assert_eq!(TypeClass::classify("VARCHAR(255)"), TypeClass::Text);
            assert_eq!(TypeClass::classify("DATETIME"), TypeClass::DateTime);
            assert_eq!(TypeClass::classify("NUMERIC(10,2)"), TypeClass::Numeric);
            assert_eq!(TypeClass::classify(""), TypeClass::Blob);
        }

        // --- phase: cell_display_formats_reals_without_trailing_zeros ---
        {
            assert_eq!(Cell::Real(3.0).display(), "3.0");
            assert_eq!(Cell::Real(3.50).display(), "3.5");
            assert_eq!(Cell::Null.display(), "NULL");
        }

        // --- phase: bounded_text_refuses_hostile_cells ---
        {
            let Cell::Text(value) =
                Cell::bounded_text("x".repeat(Cell::MAX_RETAINED_TEXT_BYTES + 99))
            else {
                unreachable!()
            };
            assert!(value.len() <= Cell::MAX_RETAINED_TEXT_BYTES + 32);
            assert!(value.ends_with("… [truncated]"));
        }

        // --- phase: sample_profile_is_connected_and_local ---
        {
            let p = ConnectionProfile::sqlite_sample();
            assert!(p.connected);
            assert!(!p.is_remote());
            assert!(p.is_sample);
        }

        // --- phase: remote_profile_flags_network_engines ---
        {
            assert!(ConnectionProfile::new("x", Engine::Postgres).is_remote());
            assert!(ConnectionProfile::new("x", Engine::MySql).is_remote());
            assert!(!ConnectionProfile::new("x", Engine::Sqlite).is_remote());
        }
    }
}
