//! What a database answers: its catalog and the rows a statement returns.
//!
//! The engine keeps no text a person reads word for word: a cell knows what it holds (a number,
//! text, nothing, a binary value) and how it shows, and the app words the counts and the notes.
//! That keeps one catalogue of words in the app, and the engine the same for every language
//! that calls it.

use serde::Serialize;

/// Which engine a connection targets. `SQLite` is a file on this device; PostgreSQL and `MySQL`
/// are network engines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Sqlite,
    #[default]
    Postgres,
    MySql,
}

impl Engine {
    /// The stable key an engine is named by where it is typed.
    #[must_use]
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
        }
    }

    /// An engine from its key, or nothing for a key this device does not run.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "sqlite" => Some(Self::Sqlite),
            "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" => Some(Self::MySql),
            _ => None,
        }
    }

    /// The port a connection to this engine defaults to; zero for the file engine.
    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Self::Sqlite => 0,
            Self::Postgres => 5432,
            Self::MySql => 3306,
        }
    }
}

/// A connection as the engine opens it: the profile the workspace's record holds, plus the
/// password this device keeps for it.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
    /// The path of a `SQLite` file on this device; empty for the other engines.
    pub file_path: String,
    /// The bundled catalog captures show, opened in memory instead of from a file.
    pub sample: bool,
}

impl Profile {
    /// A profile aimed at `engine` on this device's localhost and that engine's port: what a
    /// form starts at, and what the drivers' tests open.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            host: "localhost".to_string(),
            port: engine.default_port(),
            database: String::new(),
            user: String::new(),
            password: String::new(),
            file_path: String::new(),
            sample: false,
        }
    }

    /// Whether the engine reaches another computer, so the profile carries an address.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self.engine, Engine::Postgres | Engine::MySql)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub not_null: bool,
    pub primary_key: bool,
    pub default_value: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ForeignKey {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
}

#[derive(Clone, Debug, Serialize)]
pub struct TableInfo {
    /// The schema it stands in, which every live operation names.
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    pub indexes: Vec<IndexInfo>,
    /// Estimated row count: a `COUNT(*)` for tables, `None` for views.
    pub row_count: Option<i64>,
}

/// A relation's unambiguous catalog address: a table name is unique only inside its schema, so
/// live operations never use a bare name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelationRef {
    pub schema: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: Vec<TableInfo>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DatabaseSchema {
    pub schemas: Vec<SchemaInfo>,
}

impl DatabaseSchema {
    #[must_use]
    pub fn table_count(&self) -> usize {
        self.schemas.iter().map(|s| s.tables.len()).sum()
    }

    /// The table of that name in any schema, as a test asks for one it made.
    #[must_use]
    pub fn find_table(&self, name: &str) -> Option<&TableInfo> {
        self.schemas
            .iter()
            .flat_map(|schema| schema.tables.iter())
            .find(|table| table.name == name)
    }

    #[must_use]
    pub fn find_relation(&self, schema: &str, name: &str) -> Option<&TableInfo> {
        self.schemas
            .iter()
            .find(|candidate| candidate.name == schema)?
            .tables
            .iter()
            .find(|table| table.name == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// A binary value, kept by its size only.
    Blob(usize),
}

impl Cell {
    pub const MAX_RETAINED_TEXT_BYTES: usize = 16 * 1024;

    /// Text kept at a bounded size, so one hostile cell cannot exhaust the window.
    #[must_use]
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

    /// The payload this cell retains. A binary value keeps its size only.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::Text(value) => value.len(),
            Self::Integer(_) | Self::Real(_) => std::mem::size_of::<i64>(),
            Self::Null | Self::Blob(_) => 0,
        }
    }

    /// The grid's text; `NULL` renders as the literal token.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Null => "NULL".to_string(),
            Self::Integer(i) => i.to_string(),
            Self::Real(r) => format_real(*r),
            Self::Text(s) => s.clone(),
            Self::Blob(n) => format!("Binary, {n} bytes"),
        }
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Integer(_) | Self::Real(_))
    }

    /// The cell aligns as a number, or holds nothing, so a column of these aligns.
    #[must_use]
    pub fn is_numeric_or_null(&self) -> bool {
        self.is_numeric() || self.is_null()
    }
}

#[allow(
    clippy::float_cmp,
    reason = "trunc() is exact on r itself: this asks whether r already is a whole number"
)]
fn format_real(r: f64) -> String {
    if r == r.trunc() && r.abs() < 1e15 {
        format!("{r:.1}")
    } else {
        let s = format!("{r:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Truncation {
    /// Collection stopped at the row cap.
    RowLimit,
    /// Collection stopped at the payload cap.
    Payload,
}

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// Wall-clock time the statement took.
    pub elapsed_ms: f64,
    /// Rows affected, for a statement that changes data.
    pub rows_affected: Option<i64>,
    pub is_select: bool,
    /// Why collection stopped short, if it did, so the grid never pretends the server returned
    /// only these rows.
    pub truncation: Option<Truncation>,
}

impl QueryResult {
    /// The result as the app reads it: the columns, every row's text, and how the statement
    /// ended. A cell's text is what the grid shows, `NULL` included; `nulls` says which cells
    /// hold nothing (a copy writes those as empty), and `numeric` says which columns align as
    /// numbers — the shape JSON reads into a record of lists, with no record inside a record.
    #[must_use]
    pub fn answer(&self) -> ResultAnswer {
        let numeric = (0..self.columns.len())
            .map(|at| {
                self.rows
                    .iter()
                    .all(|row| row.get(at).is_none_or(Cell::is_numeric_or_null))
            })
            .collect();
        ResultAnswer {
            columns: self.columns.clone(),
            rows: self
                .rows
                .iter()
                .map(|row| row.iter().map(Cell::display).collect())
                .collect(),
            nulls: self
                .rows
                .iter()
                .map(|row| row.iter().map(Cell::is_null).collect())
                .collect(),
            numeric,
            rows_affected: self.rows_affected,
            is_select: self.is_select,
            elapsed_ms: self.elapsed_ms,
            truncated: match self.truncation {
                None => "",
                Some(Truncation::RowLimit) => "rows",
                Some(Truncation::Payload) => "payload",
            }
            .to_owned(),
        }
    }
}

/// A result as the app reads it.
#[derive(Clone, Debug, Serialize)]
pub struct ResultAnswer {
    pub columns: Vec<String>,
    /// Every row's cells, as the grid shows them; a `NULL` cell reads `NULL`.
    pub rows: Vec<Vec<String>>,
    /// Which cells hold nothing, in the rows' shape.
    pub nulls: Vec<Vec<bool>>,
    /// Which columns align as numbers.
    pub numeric: Vec<bool>,
    pub rows_affected: Option<i64>,
    pub is_select: bool,
    pub elapsed_ms: f64,
    /// `rows`, `payload`, or empty when the whole result is here.
    pub truncated: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn cells_show_what_they_hold_and_answer_the_app;

        phase reals_show_without_trailing_zeros_and_null_is_a_token;
        {
            assert_eq!(Cell::Real(3.0).display(), "3.0");
            assert_eq!(Cell::Real(3.50).display(), "3.5");
            assert_eq!(Cell::Null.display(), "NULL");
            assert_eq!(Cell::Blob(4).display(), "Binary, 4 bytes");
            assert!(Cell::Integer(7).is_numeric_or_null());
            assert!(Cell::Null.is_null());
            assert!(!Cell::Text("1".into()).is_null());
        }

        phase a_hostile_cell_is_kept_at_a_bounded_size;
        {
            let Cell::Text(value) = Cell::bounded_text("x".repeat(Cell::MAX_RETAINED_TEXT_BYTES + 99))
            else {
                unreachable!()
            };
            assert!(value.len() <= Cell::MAX_RETAINED_TEXT_BYTES + 32);
            assert!(value.ends_with("… [truncated]"));
        }

        phase a_result_answers_what_ended_it;
        {
            let result = QueryResult {
                columns: vec!["id".into()],
                rows: vec![vec![Cell::Integer(1)]],
                elapsed_ms: 12.4,
                is_select: true,
                truncation: Some(Truncation::RowLimit),
                ..QueryResult::default()
            };
            let answer = result.answer();
            assert_eq!(answer.columns, ["id"]);
            assert_eq!(answer.rows[0][0], "1");
            assert_eq!(answer.numeric, [true], "a column of numbers aligns");
            assert_eq!(answer.nulls, [[false]]);
            assert_eq!(answer.truncated, "rows");
            assert_eq!(answer.rows_affected, None);
        }

        phase a_schema_finds_relations_by_their_schema_and_name;
        {
            let schema = DatabaseSchema {
                schemas: vec![SchemaInfo {
                    name: "main".into(),
                    tables: vec![TableInfo {
                        name: "tracks".into(),
                        kind: RelationKind::Table,
                        columns: vec![],
                        foreign_keys: vec![],
                        indexes: vec![],
                        row_count: Some(15),
                    }],
                }],
            };
            assert_eq!(schema.table_count(), 1);
            assert!(schema.find_relation("main", "tracks").is_some());
            assert!(schema.find_relation("other", "tracks").is_none());
        }
    }
}
