//! Runtime session state: the open connections, the live engine handle, the
//! introspected schema, and the most recent query result. This is per-peer
//! view state (not in the CRDT) — only the editor buffer is collaborative.

use crate::engine::SqliteEngine;
use crate::model::{
    ConnectionProfile, DatabaseSchema, Engine, QueryResult, RelationKind, TableInfo,
};
use uuid::Uuid;

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
}

impl Session {
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
        };
        // Run the default query once so the results grid is populated on
        // first paint and in `--shots`.
        s.run_query(crate::sample::DEFAULT_QUERY);
        s
    }

    pub fn active_connection(&self) -> &ConnectionProfile {
        &self.connections[self.active]
    }

    pub fn selected_table_info(&self) -> Option<&TableInfo> {
        let name = self.selected_table.as_deref()?;
        self.schema.find_table(name)
    }

    /// Switch the active connection. For the sample SQLite engine this is a
    /// no-op reconnect; remote engines remain gated.
    pub fn select_connection(&mut self, idx: usize) {
        if idx >= self.connections.len() || idx == self.active {
            return;
        }
        self.active = idx;
        let profile = self.connections[idx].clone();
        if profile.is_sample {
            // The sample engine is already open; just re-introspect.
            if let Some(eng) = &self.engine {
                self.schema = eng.introspect().unwrap_or_default();
            }
            self.status = RunStatus::Ok(format!("Connected to {}", profile.name));
        } else {
            // Remote engines are gated; surface a clear, non-fatal message.
            self.status = RunStatus::Error(format!(
                "{} connections need a network grant — gated in this build",
                profile.engine.label()
            ));
        }
        self.selected_table = self
            .schema
            .schemas
            .first()
            .and_then(|s| s.tables.first())
            .map(|t| t.name.clone());
    }

    /// Add a new connection profile and select it.
    pub fn add_connection(&mut self, profile: ConnectionProfile) -> Uuid {
        let id = profile.id;
        self.connections.push(profile);
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
}
