//! Runtime session state: the open connections, the introspected schema, and
//! the most recent query result. This is per-peer view state (not in the CRDT)
//! — the collaborative state (the SQL buffer and the saved-connection list)
//! lives in the document; here we hold the transient result of talking to a
//! live server.
//!
//! Every blocking operation (connecting, introspecting, running SQL) is handed
//! to the background [`crate::worker::DbWorker`] so it never stalls the UI
//! thread. Requests are fire-and-forget; the replies are folded back in by
//! [`drain`](Session::drain), which the widget calls each frame. The hermetic
//! demo/`--shots` build and the headless tests drive the identical worker
//! synchronously via [`block_until_idle`](Session::block_until_idle).

use std::collections::HashMap;

use uuid::Uuid;

use crate::model::{
    ConnectionProfile, DatabaseSchema, Engine, QueryResult, RelationKind, TableInfo,
};
use crate::worker::{DbReply, DbRequest, DbWorker};

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
    /// A connect/query is in flight on the worker (shown neutral, not green).
    Busy(String),
    Ok(String),
    Error(String),
}

/// Everything the app needs at runtime that is not collaborative.
pub struct Session {
    /// Saved connection profiles shown in the sidebar. For the live app these
    /// mirror `root["connections"]`; for the demo they are seeded view state.
    pub connections: Vec<ConnectionProfile>,
    /// Index into `connections` of the active connection.
    pub active: usize,
    /// Whether a live engine is currently open on the worker for the active
    /// connection (reflected by the sidebar status dot).
    pub connected: bool,
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
    /// Bumped whenever a fresh query / data result lands, so the grids rebind.
    pub query_gen: usize,
    pub data_gen: usize,
    /// Session-only passwords keyed by profile id. Never persisted and never
    /// written to the shared document — each peer supplies its own.
    passwords: HashMap<Uuid, String>,
    /// The background engine thread.
    worker: DbWorker,
}

impl Session {
    /// The live session: it starts empty (the saved connections are read from
    /// the shared document by the widget and pushed in via
    /// [`set_connections`](Self::set_connections)). No sample database, no
    /// fabricated profiles, no live engine until the user connects.
    pub fn new() -> Self {
        Self {
            connections: Vec::new(),
            active: 0,
            connected: false,
            schema: DatabaseSchema::default(),
            selected_table: None,
            query_result: None,
            data_result: None,
            tab: MainTab::Query,
            status: RunStatus::Idle,
            data_offset: 0,
            data_page_size: 100,
            query_gen: 0,
            data_gen: 0,
            passwords: HashMap::new(),
            worker: DbWorker::spawn(),
        }
    }

    /// Build a session with the bundled sample connection live and its schema
    /// introspected, plus two disconnected example remotes so the connection
    /// manager has something to show. Drives the real worker synchronously so
    /// the `--shots` frame is fully populated on first paint.
    pub fn with_sample() -> Self {
        let mut s = Self::new();

        // The bundled sample opens for real (SQLite in-memory) and its default
        // query runs, all synchronously so one offscreen frame shows data.
        let sample = ConnectionProfile::sqlite_sample();
        s.connections.push(sample);
        s.active = 0;
        s.connect_active();
        s.block_until_idle();
        s.run_query(crate::sample::DEFAULT_QUERY);
        s.block_until_idle();

        // Two example remotes, disconnected (no password, gated on a real
        // server). They demonstrate the sidebar's multi-engine roster.
        let mut pg = ConnectionProfile::new("Analytics · prod", Engine::Postgres);
        pg.host = "db.internal".into();
        pg.database = "analytics".into();
        pg.user = "reader".into();
        s.connections.push(pg);
        let mut my = ConnectionProfile::new("Shop · staging", Engine::MySql);
        my.host = "mysql.staging".into();
        my.database = "shop".into();
        my.user = "app".into();
        s.connections.push(my);

        s
    }

    // ---- worker plumbing -------------------------------------------------

    /// Fold every reply the worker has ready into the session, without
    /// blocking. Call once per frame.
    pub fn drain(&mut self) {
        let replies = self.worker.drain();
        for reply in replies {
            self.apply_reply(reply);
        }
    }

    /// Block until every outstanding worker request has replied. Used by the
    /// demo/`--shots` build and the headless tests to await the real engine.
    pub fn block_until_idle(&mut self) {
        let replies = self.worker.block_until_idle();
        for reply in replies {
            self.apply_reply(reply);
        }
    }

    /// Whether the worker still has work in flight (drives the app's
    /// `needs_animation_frame`, so results land without extra input).
    pub fn busy(&self) -> bool {
        self.worker.busy()
    }

    fn apply_reply(&mut self, reply: DbReply) {
        match reply {
            DbReply::Connected { name, schema } => {
                self.connected = true;
                self.set_schema(schema);
                self.mark_connected(true);
                let sample = self.active_connection().is_some_and(|c| c.is_sample);
                self.status = if sample {
                    RunStatus::Idle
                } else {
                    RunStatus::Ok(format!("Connected to {name}"))
                };
            }
            DbReply::ConnectFailed { error, .. } => {
                self.connected = false;
                self.set_schema(DatabaseSchema::default());
                self.mark_connected(false);
                self.status = RunStatus::Error(error);
            }
            DbReply::Queried { result, schema } => match result {
                Ok(r) => {
                    self.status = RunStatus::Ok(r.message());
                    self.query_result = Some(r);
                    self.query_gen += 1;
                    if let Some(schema) = schema {
                        self.schema = schema;
                    }
                }
                Err(e) => self.status = RunStatus::Error(e),
            },
            DbReply::Browsed { result } => match result {
                Ok(r) => {
                    self.data_result = Some(r);
                    self.data_gen += 1;
                }
                Err(e) => self.status = RunStatus::Error(e),
            },
            DbReply::Disconnected => {
                self.connected = false;
                self.mark_connected(false);
            }
        }
    }

    /// Replace the schema and reset the selected table to the first real table.
    fn set_schema(&mut self, schema: DatabaseSchema) {
        self.schema = schema;
        self.selected_table = self
            .schema
            .schemas
            .first()
            .and_then(|s| s.tables.iter().find(|t| t.kind == RelationKind::Table))
            .map(|t| t.name.clone());
    }

    /// Reflect the live-handle state on the active profile so the sidebar
    /// status dot is honest about whether a socket is actually open.
    fn mark_connected(&mut self, connected: bool) {
        if let Some(p) = self.connections.get_mut(self.active) {
            p.connected = connected;
        }
    }

    // ---- connection management ------------------------------------------

    /// Remember a connection's password for this session only (never persisted
    /// and never written to the shared document).
    pub fn remember_password(&mut self, id: Uuid, password: String) {
        if password.is_empty() {
            self.passwords.remove(&id);
        } else {
            self.passwords.insert(id, password);
        }
    }

    /// The active profile with its in-memory password refilled, ready to hand
    /// to the worker.
    fn active_profile_with_password(&self) -> Option<ConnectionProfile> {
        let mut p = self.connections.get(self.active).cloned()?;
        if let Some(pw) = self.passwords.get(&p.id) {
            p.password = pw.clone();
        }
        Some(p)
    }

    /// Open (or reopen) the active connection on the worker. Non-blocking: the
    /// `Connected`/`ConnectFailed` reply is folded in by [`drain`](Self::drain).
    pub fn connect_active(&mut self) {
        let Some(profile) = self.active_profile_with_password() else {
            self.connected = false;
            self.set_schema(DatabaseSchema::default());
            return;
        };
        // Clear the previous connection's view state up front.
        self.connected = false;
        self.query_result = None;
        self.data_result = None;
        self.set_schema(DatabaseSchema::default());
        self.status = RunStatus::Busy(format!("Connecting to {}…", profile.name));
        self.worker.request(DbRequest::Connect(Box::new(profile)));
    }

    /// Switch the active connection and (re)open its engine on the worker.
    pub fn select_connection(&mut self, idx: usize) {
        if idx >= self.connections.len() || idx == self.active {
            return;
        }
        self.active = idx;
        self.connect_active();
    }

    /// Reconcile the connection list from the shared document. Preserves which
    /// profile is active (by id), keeps the session's passwords, and resets the
    /// view when the active connection has been removed. Returns the id of a
    /// connection that is present now but was not before (so the caller can
    /// decide whether to auto-connect a freshly-added one).
    pub fn set_connections(&mut self, mut incoming: Vec<ConnectionProfile>) {
        // Refill passwords the session already knows.
        for p in &mut incoming {
            if let Some(pw) = self.passwords.get(&p.id) {
                p.password = pw.clone();
            }
        }
        let active_id = self.connections.get(self.active).map(|c| c.id);
        self.connections = incoming;
        // Keep the same connection active if it still exists.
        if let Some(pos) = active_id.and_then(|id| self.connections.iter().position(|c| c.id == id))
        {
            self.active = pos;
        } else {
            self.active = 0;
            // The active connection vanished: drop its stale view state.
            self.connected = false;
            self.query_result = None;
            self.data_result = None;
            self.set_schema(DatabaseSchema::default());
        }
    }

    /// Add a profile as live view state and connect it, blocking until the
    /// engine has opened. Used by the demo and the headless integration tests
    /// (the live app adds connections through the action model instead).
    pub fn add_connection_blocking(&mut self, mut profile: ConnectionProfile) -> Uuid {
        let id = profile.id;
        let password = std::mem::take(&mut profile.password);
        self.remember_password(id, password);
        self.connections.push(profile);
        self.active = self.connections.len() - 1;
        self.connect_active();
        self.block_until_idle();
        id
    }

    // ---- introspection accessors ----------------------------------------

    /// The active connection profile, or `None` when there are no saved
    /// connections yet (a fresh live session).
    pub fn active_connection(&self) -> Option<&ConnectionProfile> {
        self.connections.get(self.active)
    }

    pub fn selected_table_info(&self) -> Option<&TableInfo> {
        let name = self.selected_table.as_deref()?;
        self.schema.find_table(name)
    }

    // ---- query + browse -------------------------------------------------

    /// Run the SQL in the editor against the active engine (non-blocking). The
    /// result is folded in by [`drain`](Self::drain).
    pub fn run_query(&mut self, sql: &str) {
        if self.active_connection().is_none() {
            self.status = RunStatus::Error("No live connection".into());
            return;
        }
        self.status = RunStatus::Busy("Running…".into());
        self.worker.request(DbRequest::Query(sql.to_string()));
    }

    /// Select a table by name and load its first page of data.
    pub fn select_table(&mut self, name: &str) {
        self.selected_table = Some(name.to_string());
        self.data_offset = 0;
        self.load_table_data();
    }

    /// Request the current page of rows for the selected table (non-blocking).
    pub fn load_table_data(&mut self) {
        let Some(table) = self.selected_table.clone() else {
            return;
        };
        if self.active_connection().is_none() {
            return;
        }
        self.worker.request(DbRequest::Browse {
            table,
            limit: self.data_page_size,
            offset: self.data_offset,
        });
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
        assert!(s.connected, "the sample engine connected");
        assert!(s.schema.table_count() >= 4);
        let r = s.query_result.as_ref().expect("default query ran");
        assert!(r.is_select);
        assert!(!r.rows.is_empty());
    }

    #[test]
    fn selecting_table_loads_its_data() {
        let mut s = Session::with_sample();
        s.select_table("tracks");
        s.block_until_idle();
        let d = s.data_result.as_ref().expect("table data loaded");
        assert!(d.is_select);
        assert!(!d.columns.is_empty());
    }

    #[test]
    fn remote_connection_to_unreachable_server_errors_honestly() {
        // A real Postgres connect to a closed port fails through the driver on
        // the worker: no live engine, no fabricated schema, and an honest error
        // banner — never a faked "connected" state.
        let mut s = Session::new();
        let mut pg = ConnectionProfile::new("Unreachable pg", Engine::Postgres);
        pg.host = "127.0.0.1".into();
        pg.port = 1; // reserved/closed — connection is refused immediately.
        pg.database = "nope".into();
        pg.user = "nobody".into();
        s.add_connection_blocking(pg);

        assert!(
            !s.connected,
            "no live engine when the server is unreachable"
        );
        assert_eq!(s.schema.table_count(), 0, "no fabricated schema on failure");
        assert!(
            !s.connections[s.active].connected,
            "profile is not marked connected on failure"
        );
        match &s.status {
            RunStatus::Error(_) => {}
            other => panic!("expected an honest connection error, got {other:?}"),
        }
    }

    #[test]
    fn live_session_starts_empty() {
        // A fresh live session fabricates nothing: no sample database, no
        // connection profiles, no live engine.
        let s = Session::new();
        assert!(
            s.connections.is_empty(),
            "no fabricated connection profiles"
        );
        assert!(!s.connected, "no live engine on a fresh machine");
        assert!(s.active_connection().is_none(), "nothing active when empty");
        assert!(s.query_result.is_none(), "no canned query result");
        assert_eq!(s.schema.table_count(), 0, "no fabricated schema");
    }

    #[test]
    fn added_sqlite_connection_opens_for_real() {
        let dir = std::env::temp_dir().join(format!("db-client-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_file = dir.join("app.db");

        let mut s = Session::new();
        let mut profile = ConnectionProfile::new("Local app DB", Engine::Sqlite);
        profile.file_path = db_file.to_string_lossy().into_owned();
        s.add_connection_blocking(profile);

        assert!(s.connected, "a local SQLite file connects for real");
        assert_eq!(s.connections.len(), 1);
    }

    #[test]
    fn set_connections_preserves_the_active_id() {
        let mut s = Session::new();
        let a = ConnectionProfile::new("A", Engine::Postgres);
        let b = ConnectionProfile::new("B", Engine::MySql);
        s.set_connections(vec![a.clone(), b.clone()]);
        s.active = 1; // B is active
        // A reorder that keeps both keeps B active by id.
        s.set_connections(vec![b.clone(), a.clone()]);
        assert_eq!(s.active_connection().unwrap().id, b.id);
        // Removing the active one resets to the first + clears view state.
        s.set_connections(vec![a.clone()]);
        assert_eq!(s.active_connection().unwrap().id, a.id);
        assert!(!s.connected);
    }

    #[test]
    fn passwords_are_refilled_but_kept_out_of_profiles_list() {
        let mut s = Session::new();
        let p = ConnectionProfile::new("A", Engine::Postgres);
        s.remember_password(p.id, "secret".into());
        // set_connections refills the password onto the in-memory profile...
        s.set_connections(vec![p]);
        assert_eq!(s.connections[0].password, "secret");
        // ...but a profile with no remembered password stays empty.
        let q = ConnectionProfile::new("B", Engine::MySql);
        s.set_connections(vec![q]);
        assert!(s.connections[0].password.is_empty());
    }
}
