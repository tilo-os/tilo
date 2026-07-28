//! Runtime session state: the open connections, the live engine handle, the
//! introspected schema, and the most recent query result. This is per-peer
//! view state (not in the CRDT) — only the editor buffer is collaborative.

use std::path::{Path, PathBuf};

use crate::engine::SqliteEngine;
use crate::model::{
    ConnectionProfile, DatabaseSchema, Engine, QueryResult, RelationKind, TableInfo,
};
use uuid::Uuid;

/// Resolve the directory that holds the user's saved connection profiles.
/// `$QOS_DB_CLIENT_DIR` overrides it (used by tests); otherwise it is
/// `$XDG_CONFIG_HOME/qos/db_client` (falling back to `~/.config/...`).
fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("QOS_DB_CLIENT_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("qos").join("db_client")
}

/// Load the saved (non-sample) connection profiles from `dir`, or an empty
/// list if none have been saved yet / the file is unreadable.
fn load_saved_connections(dir: &Path) -> Vec<ConnectionProfile> {
    let path = dir.join("connections.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<ConnectionProfile>>(&bytes).unwrap_or_default()
}

/// Persist the user's connection profiles to `dir`. The in-memory sample is
/// never written; live connections are reset to disconnected on disk.
fn save_connections(dir: &Path, connections: &[ConnectionProfile]) {
    let saved: Vec<ConnectionProfile> = connections
        .iter()
        .filter(|c| !c.is_sample)
        .cloned()
        .map(|mut c| {
            c.connected = false;
            c
        })
        .collect();
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_vec_pretty(&saved) {
        let _ = std::fs::write(dir.join("connections.json"), json);
    }
}

/// Which workspace tab the main panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainTab {
    /// SQL editor + results grid.
    Query,
    /// Browse rows of the selected table.
    Data,
    /// Columns / keys / indexes of the selected table.
    Structure,
}

impl MainTab {
    pub fn label(self) -> &'static str {
        match self {
            MainTab::Query => "Query",
            MainTab::Data => "Data",
            MainTab::Structure => "Structure",
        }
    }

    pub fn all() -> [MainTab; 3] {
        [MainTab::Query, MainTab::Data, MainTab::Structure]
    }
}

/// Outcome banner shown under the editor / data grid.
#[derive(Clone, Debug)]
pub enum RunStatus {
    Idle,
    Ok(String),
    Error(String),
}

/// Everything the app needs at runtime that is not collaborative.
pub struct Session {
    /// Saved connection profiles shown in the sidebar.
    pub connections: Vec<ConnectionProfile>,
    /// Index into `connections` of the active connection.
    pub active: usize,
    /// Live SQLite handle for the active connection (None for remote
    /// engines that have not been granted an outbound socket).
    pub engine: Option<SqliteEngine>,
    /// Introspected schema for the active connection.
    pub schema: DatabaseSchema,
    /// The table currently selected in the schema tree, by name.
    pub selected_table: Option<String>,
    /// Result of the most recent `Run` in the Query tab.
    pub query_result: Option<QueryResult>,
    /// Result of browsing the selected table in the Data tab.
    pub data_result: Option<QueryResult>,
    /// Active main-panel tab.
    pub tab: MainTab,
    /// Status banner.
    pub status: RunStatus,
    /// Pagination offset for the Data tab.
    pub data_offset: usize,
    /// Page size for the Data tab.
    pub data_page_size: usize,
    /// Directory the user's saved connections persist to. `None` for the
    /// hermetic demo/sample session, which never touches disk.
    data_dir: Option<PathBuf>,
}

impl Session {
    /// The live session: it loads the user's saved connections from disk and
    /// starts empty on a fresh machine. No sample database and no fabricated
    /// connection profiles — the sample lives only in [`Session::with_sample`].
    pub fn new() -> Self {
        Self::load_from(default_data_dir())
    }

    /// Live session backed by a specific data directory (kept separate so
    /// tests can point persistence at a temp dir without touching `$HOME`).
    pub fn load_from(dir: PathBuf) -> Self {
        let connections = load_saved_connections(&dir);
        let mut s = Self {
            connections,
            active: 0,
            engine: None,
            schema: DatabaseSchema::default(),
            selected_table: None,
            query_result: None,
            data_result: None,
            tab: MainTab::Query,
            status: RunStatus::Idle,
            data_offset: 0,
            data_page_size: 100,
            data_dir: Some(dir),
        };
        // Open the active connection if it is a real local SQLite file.
        if !s.connections.is_empty() {
            s.connect_active();
        }
        s
    }

    /// (Re)open the active connection's engine. Local SQLite files open for
    /// real; remote engines stay gated behind a sandbox grant.
    fn connect_active(&mut self) {
        let Some(profile) = self.connections.get(self.active).cloned() else {
            self.engine = None;
            self.schema = DatabaseSchema::default();
            return;
        };
        if profile.is_sample {
            if let Ok(eng) = SqliteEngine::open_sample() {
                self.schema = eng.introspect().unwrap_or_default();
                self.engine = Some(eng);
            }
        } else if profile.engine == Engine::Sqlite && !profile.file_path.is_empty() {
            match SqliteEngine::open_file(&profile.file_path) {
                Ok(eng) => {
                    self.schema = eng.introspect().unwrap_or_default();
                    self.engine = Some(eng);
                    self.status = RunStatus::Ok(format!("Connected to {}", profile.name));
                }
                Err(e) => {
                    self.engine = None;
                    self.schema = DatabaseSchema::default();
                    self.status = RunStatus::Error(e);
                }
            }
        } else {
            // Remote engines are gated; surface a clear, non-fatal message.
            self.engine = None;
            self.schema = DatabaseSchema::default();
            self.status = RunStatus::Error(format!(
                "{} connections need a network grant — gated in this build",
                profile.engine.label()
            ));
        }
        self.selected_table = self
            .schema
            .schemas
            .first()
            .and_then(|s| s.tables.iter().find(|t| t.kind == RelationKind::Table))
            .map(|t| t.name.clone());
    }
    /// Build a session with the bundled sample connection live and its
    /// schema introspected. Falls back to an empty schema on engine error
    /// so the UI always has something to render.
    pub fn with_sample() -> Self {
        let sample = ConnectionProfile::sqlite_sample();
        let (engine, schema) = match SqliteEngine::open_sample() {
            Ok(eng) => {
                let schema = eng.introspect().unwrap_or_default();
                (Some(eng), schema)
            }
            Err(_) => (None, DatabaseSchema::default()),
        };

        let selected_table = schema
            .schemas
            .first()
            .and_then(|s| s.tables.iter().find(|t| t.kind == RelationKind::Table))
            .map(|t| t.name.clone());

        // Pre-seed a few extra (disconnected) remote profiles so the
        // connection manager has something to show. They stay gated until a
        // network grant arrives.
        let mut connections = vec![sample];
        let mut pg = ConnectionProfile::new("Analytics · prod", Engine::Postgres);
        pg.host = "db.internal".into();
        pg.database = "analytics".into();
        pg.user = "reader".into();
        connections.push(pg);
        let mut my = ConnectionProfile::new("Shop · staging", Engine::MySql);
        my.host = "mysql.staging".into();
        my.database = "shop".into();
        my.user = "app".into();
        connections.push(my);

        let mut s = Self {
            connections,
            active: 0,
            engine,
            schema,
            selected_table,
            query_result: None,
            data_result: None,
            tab: MainTab::Query,
            status: RunStatus::Idle,
            data_offset: 0,
            data_page_size: 100,
            data_dir: None,
        };
        // Run the default query once so the results grid is populated on
        // first paint and in `--shots`.
        s.run_query(crate::sample::DEFAULT_QUERY);
        s
    }

    /// The active connection profile, or `None` when there are no saved
    /// connections yet (a fresh live session).
    pub fn active_connection(&self) -> Option<&ConnectionProfile> {
        self.connections.get(self.active)
    }

    pub fn selected_table_info(&self) -> Option<&TableInfo> {
        let name = self.selected_table.as_deref()?;
        self.schema.find_table(name)
    }

    /// Switch the active connection and (re)open its engine. Local SQLite
    /// files connect for real; remote engines remain gated.
    pub fn select_connection(&mut self, idx: usize) {
        if idx >= self.connections.len() || idx == self.active {
            return;
        }
        self.active = idx;
        self.connect_active();
    }

    /// Add a new connection profile, persist it, select it and (for a local
    /// SQLite file) open it.
    pub fn add_connection(&mut self, profile: ConnectionProfile) -> Uuid {
        let id = profile.id;
        self.connections.push(profile);
        self.active = self.connections.len() - 1;
        self.connect_active();
        if let Some(dir) = self.data_dir.clone() {
            save_connections(&dir, &self.connections);
        }
        id
    }

    /// Select a table by name and load its first page of data.
    pub fn select_table(&mut self, name: &str) {
        self.selected_table = Some(name.to_string());
        self.data_offset = 0;
        self.load_table_data();
    }

    /// Run the SQL in the editor against the active engine, storing the
    /// result and updating the status banner.
    pub fn run_query(&mut self, sql: &str) {
        let Some(engine) = &self.engine else {
            self.status = RunStatus::Error("No live connection".into());
            return;
        };
        match engine.run(sql) {
            Ok(result) => {
                self.status = RunStatus::Ok(result.message());
                self.query_result = Some(result);
                // A DDL/DML statement may have changed the schema.
                if let Some(eng) = &self.engine {
                    self.schema = eng.introspect().unwrap_or_default();
                }
            }
            Err(e) => {
                self.status = RunStatus::Error(e);
            }
        }
    }

    /// Load the current page of rows for the selected table into
    /// `data_result`.
    pub fn load_table_data(&mut self) {
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        let Some(engine) = &self.engine else { return };
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT {} OFFSET {}",
            table.replace('"', "\"\""),
            self.data_page_size,
            self.data_offset
        );
        match engine.run(&sql) {
            Ok(result) => self.data_result = Some(result),
            Err(e) => self.status = RunStatus::Error(e),
        }
    }

    /// Total rows in the selected table (from introspection), for paging.
    pub fn selected_table_rows(&self) -> i64 {
        self.selected_table_info()
            .and_then(|t| t.row_count)
            .unwrap_or(0)
    }

    pub fn next_data_page(&mut self) {
        let total = self.selected_table_rows() as usize;
        if self.data_offset + self.data_page_size < total {
            self.data_offset += self.data_page_size;
            self.load_table_data();
        }
    }

    pub fn prev_data_page(&mut self) {
        if self.data_offset >= self.data_page_size {
            self.data_offset -= self.data_page_size;
        } else {
            self.data_offset = 0;
        }
        self.load_table_data();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_seeds_sample_and_runs_default_query() {
        let s = Session::with_sample();
        assert!(s.engine.is_some());
        assert!(s.schema.table_count() >= 4);
        let r = s.query_result.as_ref().expect("default query ran");
        assert!(r.is_select);
        assert!(!r.rows.is_empty());
    }

    #[test]
    fn selecting_table_loads_its_data() {
        let mut s = Session::with_sample();
        s.select_table("tracks");
        let d = s.data_result.as_ref().expect("table data loaded");
        assert!(d.is_select);
        assert!(!d.columns.is_empty());
    }

    #[test]
    fn remote_connection_is_gated() {
        let mut s = Session::with_sample();
        // index 1 is the seeded Postgres profile.
        s.select_connection(1);
        match &s.status {
            RunStatus::Error(m) => assert!(m.contains("grant")),
            _ => panic!("expected gated error"),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("db-client-{tag}-{}", Uuid::new_v4()))
    }

    #[test]
    fn live_session_starts_empty() {
        // A fresh live session over an empty data dir fabricates nothing: no
        // sample database, no connection profiles, no live engine.
        let s = Session::load_from(temp_dir("empty"));
        assert!(s.connections.is_empty(), "no fabricated connection profiles");
        assert!(s.engine.is_none(), "no live engine on a fresh machine");
        assert!(s.active_connection().is_none(), "nothing active when empty");
        assert!(s.query_result.is_none(), "no canned query result");
        assert_eq!(s.schema.table_count(), 0, "no fabricated schema");
    }

    #[test]
    fn added_sqlite_connection_opens_persists_and_reloads() {
        let dir = temp_dir("persist");
        let db_file = dir.join("app.db");
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = Session::load_from(dir.clone());
        let mut profile = ConnectionProfile::new("Local app DB", Engine::Sqlite);
        profile.file_path = db_file.to_string_lossy().into_owned();
        s.add_connection(profile);

        // The real local SQLite engine opened (not gated like pg/mysql).
        assert!(s.engine.is_some(), "a local SQLite file connects for real");
        assert_eq!(s.connections.len(), 1);

        // A brand-new live session over the same dir loads the saved
        // connection back from disk and reconnects it.
        let reloaded = Session::load_from(dir);
        assert_eq!(reloaded.connections.len(), 1, "saved connection reloads from disk");
        assert_eq!(reloaded.connections[0].name, "Local app DB");
        assert!(reloaded.engine.is_some(), "reloaded SQLite connection reopens");
    }
}
