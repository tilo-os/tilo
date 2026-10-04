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
    ConnectionProfile, DatabaseSchema, Engine, QueryResult, RelationKind, RelationRef, TableInfo,
};
use crate::worker::{CancelQueryResult, DbReply, DbRequest, DbWorker};

/// Which workspace tab the main panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MainTab {
    /// SQL editor + results table.
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

/// A bounded per-peer record of a completed live query. It is discoverable as
/// a local resource, but never replicated: result data can reveal a server or
/// rows another collaborator is not authorized to access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryHistoryEntry {
    /// Stable local connection identity. History remains local, but this keeps
    /// its resource hierarchy truthful after the user changes connections.
    pub connection_id: Uuid,
    pub generation: usize,
    pub sql: String,
    pub summary: String,
}

const QUERY_HISTORY_LIMIT: usize = 20;

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
    /// The relation currently selected in the schema tree, qualified by its
    /// schema so duplicate names cannot cross-target a live browse.
    pub selected_relation: Option<RelationRef>,
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
    /// Bounded local history used for search/resource discovery.
    pub query_history: Vec<QueryHistoryEntry>,
    pub data_gen: usize,
    /// Session-only passwords keyed by profile id. Never persisted and never
    /// written to the shared document — each peer supplies its own.
    passwords: HashMap<Uuid, String>,
    /// Monotonic identity for a worker query. A cancellation and a reply must
    /// both match this value as well as the connection UUID.
    next_query_generation: u64,
    active_query: Option<(Uuid, u64)>,
    active_query_sql: String,
    cancelled_query: Option<(Uuid, u64)>,
    /// The background engine thread.
    worker: DbWorker,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
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
            selected_relation: None,
            query_result: None,
            data_result: None,
            tab: MainTab::Query,
            status: RunStatus::Idle,
            data_offset: 0,
            data_page_size: 100,
            query_gen: 0,
            query_history: Vec::new(),
            data_gen: 0,
            passwords: HashMap::new(),
            next_query_generation: 0,
            active_query: None,
            active_query_sql: String::new(),
            cancelled_query: None,
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
        let mut pg = ConnectionProfile::new("Production analytics", Engine::Postgres);
        pg.host = "db.internal".into();
        pg.database = "analytics".into();
        pg.user = "reader".into();
        s.connections.push(pg);
        let mut my = ConnectionProfile::new("Staging shop", Engine::MySql);
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
            DbReply::Connected {
                connection_id,
                name,
                schema,
            } => self.apply_connected_reply(connection_id, &name, schema),
            DbReply::ConnectFailed {
                connection_id,
                error,
                ..
            } => self.apply_connect_failed_reply(connection_id, error),
            DbReply::Queried {
                connection_id,
                generation,
                result,
                schema,
            } => self.apply_query_reply(connection_id, generation, result, schema),
            DbReply::Browsed {
                connection_id,
                schema,
                table,
                offset,
                result,
            } => self.apply_browse_reply(connection_id, schema, table, offset, result),
            DbReply::Disconnected => {
                self.connected = false;
                self.mark_connected(false);
            }
        }
    }

    fn apply_connected_reply(&mut self, connection_id: Uuid, name: &str, schema: DatabaseSchema) {
        if self.active_connection_id() != Some(connection_id) {
            return;
        }
        self.connected = true;
        self.set_schema(schema);
        self.mark_connected(true);
        self.status = if self.active_connection().is_some_and(|c| c.is_sample) {
            RunStatus::Idle
        } else {
            RunStatus::Ok(format!("Connected to {name}"))
        };
    }

    fn apply_connect_failed_reply(&mut self, connection_id: Uuid, error: String) {
        if self.active_connection_id() != Some(connection_id) {
            return;
        }
        self.connected = false;
        self.set_schema(DatabaseSchema::default());
        self.mark_connected(false);
        self.status = RunStatus::Error(error);
    }

    fn apply_query_reply(
        &mut self,
        connection_id: Uuid,
        generation: u64,
        result: Result<QueryResult, String>,
        schema: Option<DatabaseSchema>,
    ) {
        let identity = (connection_id, generation);
        if self.cancelled_query == Some(identity) {
            self.cancelled_query = None;
            let was_active = self.active_query == Some(identity);
            if was_active {
                self.active_query = None;
            }
            if was_active && self.active_connection_id() == Some(connection_id) {
                self.status = RunStatus::Idle;
            }
            return;
        }
        if self.active_query == Some(identity) {
            self.active_query = None;
        }
        if self.active_connection_id() != Some(connection_id) {
            return;
        }
        match result {
            Ok(result) => self.accept_query_result(connection_id, result, schema),
            Err(error) => self.status = RunStatus::Error(error),
        }
    }

    fn accept_query_result(
        &mut self,
        connection_id: Uuid,
        result: QueryResult,
        schema: Option<DatabaseSchema>,
    ) {
        let summary = result.message();
        self.status = RunStatus::Ok(summary.clone());
        self.query_result = Some(result);
        self.query_gen += 1;
        self.query_history.push(QueryHistoryEntry {
            connection_id,
            generation: self.query_gen,
            sql: std::mem::take(&mut self.active_query_sql),
            summary,
        });
        if self.query_history.len() > QUERY_HISTORY_LIMIT {
            self.query_history.remove(0);
        }
        if let Some(schema) = schema {
            self.schema = schema;
        }
    }

    fn apply_browse_reply(
        &mut self,
        connection_id: Uuid,
        schema: String,
        table: String,
        offset: usize,
        result: Result<QueryResult, String>,
    ) {
        if self.active_connection_id() != Some(connection_id)
            || self.selected_relation.as_ref()
                != Some(&RelationRef {
                    schema,
                    name: table,
                })
            || self.data_offset != offset
        {
            return;
        }
        match result {
            Ok(result) => {
                self.data_result = Some(result);
                self.data_gen += 1;
            }
            Err(error) => self.status = RunStatus::Error(error),
        }
    }

    /// Replace the schema and reset the selected table to the first real table.
    fn set_schema(&mut self, schema: DatabaseSchema) {
        self.schema = schema;
        self.selected_relation = self.schema.schemas.first().and_then(|schema| {
            schema
                .tables
                .iter()
                .find(|table| table.kind == RelationKind::Table)
                .map(|table| RelationRef {
                    schema: schema.name.clone(),
                    name: table.name.clone(),
                })
        });
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
        self.active_query = None;
        self.cancelled_query = None;
        self.query_result = None;
        self.data_result = None;
        self.set_schema(DatabaseSchema::default());
        self.status = RunStatus::Busy(format!("Connecting to {}…", profile.name));
        self.worker.request(DbRequest::Connect(Box::new(profile)));
    }

    /// Make the connection called `name` the active one and leave it
    /// unopened, as a connect that failed with `message` does.
    pub fn fail_connecting(&mut self, name: &str, message: &str) {
        if let Some(index) = self.connections.iter().position(|c| c.name == name) {
            self.active = index;
        }
        self.connected = false;
        self.active_query = None;
        self.query_result = None;
        self.query_gen += 1;
        self.data_result = None;
        self.data_gen += 1;
        self.set_schema(DatabaseSchema::default());
        self.status = RunStatus::Error(message.to_owned());
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
        // A removed profile must also evict its session-only secret. This is
        // not persistence (passwords never leave this process), but avoids
        // retaining credentials for a connection the shared document no
        // longer authorizes us to target.
        self.passwords
            .retain(|id, _| incoming.iter().any(|profile| profile.id == *id));
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

    /// Stable identity of the active profile, used to reject replies produced
    /// for a connection that has since been removed or deselected.
    pub fn active_connection_id(&self) -> Option<Uuid> {
        self.active_connection().map(|profile| profile.id)
    }

    pub fn selected_table_info(&self) -> Option<&TableInfo> {
        let relation = self.selected_relation.as_ref()?;
        self.schema.find_relation(&relation.schema, &relation.name)
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
        let Some(connection_id) = self.active_connection_id() else {
            self.status = RunStatus::Error("No live connection".into());
            return;
        };
        if self.active_query.is_some() {
            self.status = RunStatus::Error("A query is already running".into());
            return;
        }
        let Some(generation) = self.next_query_generation.checked_add(1) else {
            // Query generations are authority tokens shared by execution,
            // cancellation, and reply settlement. Reusing zero after the
            // counter wraps could let a late reply for an ancient query
            // acquire ownership of a newly-started one, so exhaustion fails
            // closed and asks for a fresh process instead.
            self.status =
                RunStatus::Error("Query identity space exhausted; restart Database Client".into());
            return;
        };
        self.next_query_generation = generation;
        self.active_query = Some((connection_id, generation));
        self.active_query_sql = sql.to_string();
        self.worker.request(DbRequest::Query {
            connection_id,
            generation,
            sql: sql.to_string(),
        });
    }

    /// Request an engine-native interrupt for the exact active query. The
    /// worker validates connection + generation before it selects a private
    /// cancellation capability, so this can never target a newer query.
    pub fn cancel_in_flight_query(&mut self) -> bool {
        let Some((connection_id, generation)) = self.active_query else {
            return false;
        };
        if self.worker.cancel_query(connection_id, generation) == CancelQueryResult::Requested {
            self.cancelled_query = Some((connection_id, generation));
            self.status = RunStatus::Busy("Cancelling query…".into());
            true
        } else {
            false
        }
    }

    /// Select a table by name and load its first page of data.
    pub fn select_table(&mut self, name: &str) {
        let Some(relation) = self.schema.relation_named(name) else {
            return;
        };
        self.select_relation(relation);
    }

    /// Select a schema-qualified relation and request its first data page.
    pub fn select_relation(&mut self, relation: RelationRef) {
        self.selected_relation = Some(relation);
        self.data_offset = 0;
        self.load_table_data();
    }

    /// Request the current page of rows for the selected table (non-blocking).
    pub fn load_table_data(&mut self) {
        let Some(relation) = self.selected_relation.clone() else {
            return;
        };
        let Some(connection_id) = self.active_connection_id() else {
            return;
        };
        self.worker.request(DbRequest::Browse {
            connection_id,
            schema: relation.schema,
            table: relation.name,
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

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

    /// Sessions seed only in demo, start empty live, open `SQLite` for real, load tables and report unreachable servers honestly.
    #[test]
    fn session_opens_connections_and_loads_tables_honestly() {
        // --- phase: session_seeds_sample_and_runs_default_query ---
        {
            let s = Session::with_sample();
            assert!(s.connected, "the sample engine connected");
            assert!(s.schema.table_count() >= 4);
            let r = s.query_result.as_ref().expect("default query ran");
            assert!(r.is_select);
            assert!(!r.rows.is_empty());
        }

        // --- phase: selecting_table_loads_its_data ---
        {
            let mut s = Session::with_sample();
            s.select_table("tracks");
            s.block_until_idle();
            let d = s.data_result.as_ref().expect("table data loaded");
            assert!(d.is_select);
            assert!(!d.columns.is_empty());
        }

        // --- phase: remote_connection_to_unreachable_server_errors_honestly ---
        {
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

        // --- phase: live_session_starts_empty ---
        {
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

        // --- phase: added_sqlite_connection_opens_for_real ---
        {
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
    }

    /// Setting connections preserves the active id and passwords are refilled but kept out of the profiles list.
    #[test]
    fn session_profiles_keep_active_id_and_secrets_out() {
        // --- phase: set_connections_preserves_the_active_id ---
        {
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

        // --- phase: passwords_are_refilled_but_kept_out_of_profiles_list ---
        {
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

    /// Stale, cancelled and exhausted query generations and stale table pages are refused, and `SQLite` cancellation releases the worker.
    #[test]
    fn query_generations_fence_stale_and_cancelled_replies() {
        // --- phase: stale_query_reply_is_ignored_after_switching_connections ---
        {
            let mut s = Session::new();
            let a = ConnectionProfile::new("A", Engine::Sqlite);
            let b = ConnectionProfile::new("B", Engine::Sqlite);
            s.set_connections(vec![a.clone(), b]);
            s.active = 1;

            s.apply_reply(DbReply::Queried {
                connection_id: a.id,
                generation: 1,
                result: Ok(QueryResult {
                    columns: vec!["stale".into()],
                    ..Default::default()
                }),
                schema: None,
            });

            assert!(s.query_result.is_none(), "a reply for A cannot overwrite B");
            assert_eq!(s.query_gen, 0);
        }

        // --- phase: cancelled_reply_cannot_clear_a_newer_query_generation ---
        {
            let mut s = Session::new();
            let profile = ConnectionProfile::new("A", Engine::Sqlite);
            let id = profile.id;
            s.set_connections(vec![profile]);
            s.active_query = Some((id, 2));
            s.cancelled_query = Some((id, 1));
            s.status = RunStatus::Busy("Running…".into());
            s.apply_reply(DbReply::Queried {
                connection_id: id,
                generation: 1,
                result: Err("interrupted".into()),
                schema: None,
            });
            assert_eq!(s.active_query, Some((id, 2)));
            assert!(matches!(s.status, RunStatus::Busy(_)));
        }

        // --- phase: exhausted_query_generation_refuses_without_reusing_authority ---
        {
            let mut s = Session::new();
            s.set_connections(vec![ConnectionProfile::new("A", Engine::Sqlite)]);
            s.next_query_generation = u64::MAX;

            s.run_query("SELECT 1");

            assert_eq!(s.next_query_generation, u64::MAX);
            assert!(s.active_query.is_none());
            assert!(matches!(
                s.status,
                RunStatus::Error(ref message) if message.contains("identity space exhausted")
            ));
        }

        // --- phase: sqlite_cancellation_interrupts_the_worker_and_releases_busy_state ---
        {
            let mut s = Session::with_sample();
            s.run_query(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 1000000000) \
             SELECT sum(x) FROM n",
        );
            let mut requested = false;
            let deadline = ui::time::now() + HANG_BOUND;
            while ui::time::now() < deadline {
                if s.cancel_in_flight_query() {
                    requested = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert!(requested, "the worker must publish the active SQLite query");
            s.block_until_idle();
            assert!(!s.busy());
            assert!(matches!(s.status, RunStatus::Idle));
        }

        // --- phase: stale_table_page_is_ignored_after_selection_changes ---
        {
            let mut s = Session::with_sample();
            let id = s.active_connection_id().unwrap();
            s.selected_relation = Some(RelationRef {
                schema: "main".into(),
                name: "artists".into(),
            });
            s.data_offset = 100;

            s.apply_reply(DbReply::Browsed {
                connection_id: id,
                schema: "main".into(),
                table: "tracks".into(),
                offset: 0,
                result: Ok(QueryResult::default()),
            });

            assert!(s.data_result.is_none(), "a stale table page must not flash");
        }
    }
}
