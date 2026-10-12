//! The database client's controller. It owns the [`Session`] (connections,
//! engine, schema, results), the collaborative document edits and every
//! effect, and presents them through the window shell: a sidebar of
//! connections and tables, and a workspace of tabs.
//!
//! The controller never draws. Each frame it publishes what is on screen as a
//! [`ViewModel`] and applies the [`Intent`]s the views asked for (see
//! [`crate::link`]); the views are the toolkit's own widgets.

use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

#[cfg(test)]
use automerge::AutoCommit;
use ui::framework::{Dispatcher, EffectReply, LiveRow, Submit, SurfaceCx};
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use uuid::Uuid;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::tilo_actions::Resource;

use crate::link::{ConnectionRow, Dialog, Intent, Link, ViewModel};
use crate::model::{DatabaseSchema, QueryResult, TableInfo};
use crate::navigator::Navigator;
use crate::record::{SavedConnection, Workspace as Record, id_of, uuid_of};
use crate::resource::DbEffect;
use crate::session::{MainTab, RunStatus, Session};
use crate::workspace::Workspace;

/// What a saved connection's keyring entry holds.
const PASSWORD_PURPOSE: &str = "password";

/// `resource`, a live catalog entity, as the row the surface publishes.
fn live_row(resource: Resource) -> LiveRow {
    let mut row = LiveRow::new(resource.id.as_str(), resource.kind, resource.label);
    for action in resource.actions {
        row = row.operation(action);
    }
    for child in resource.children {
        row = row.row(live_row(child));
    }
    row
}

/// Encode arbitrary catalog text into a path component without the ambiguity
/// of delimiter-joined ids (`a:b` vs `a`, `b`). Labels remain human-readable;
/// only machine identities use this representation.
fn resource_component(value: &str) -> String {
    hex::encode(value)
}

pub(crate) fn catalog_resource_id(kind: &str, components: &[&str]) -> String {
    std::iter::once(kind.to_string())
        .chain(
            components
                .iter()
                .map(|component| resource_component(component)),
        )
        .collect::<Vec<_>>()
        .join("/")
}

/// The catalog's structural fingerprint: the views rebuild their rows only
/// when it changes.
fn schema_signature(schema: &DatabaseSchema) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for info in &schema.schemas {
        info.name.hash(&mut hasher);
        for table in &info.tables {
            table.name.hash(&mut hasher);
            table.row_count.hash(&mut hasher);
            matches!(table.kind, crate::model::RelationKind::View).hash(&mut hasher);
            for column in &table.columns {
                column.name.hash(&mut hasher);
                column.decl_type.hash(&mut hasher);
                column.primary_key.hash(&mut hasher);
                column.not_null.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// What the views were last handed, so a result or a catalog is shared with
/// them once rather than cloned every frame.
#[derive(Default)]
struct Published {
    schema: Option<(u64, Rc<DatabaseSchema>)>,
    schema_revision: u64,
    query: Option<(usize, Option<Arc<QueryResult>>)>,
    data: Option<(usize, Option<Arc<QueryResult>>)>,
    table: Option<(crate::model::RelationRef, u64, Rc<TableInfo>)>,
}

pub struct DbClientApp {
    /// The window: connections and tables beside the workspace.
    shell: NavShell,
    link: Link,
    session: Session,
    /// Where every change of the shared record goes: the saved connections
    /// and queries and the SQL buffer, each a field edit or a verb, one undo
    /// step each.
    actions: Option<Dispatcher<Record>>,
    /// This device's keyring for connection passwords, one secret per saved
    /// profile: the document names the profile, the password stays here.
    secrets: Option<ui::secrets::SecretStore>,
    /// Id of a connection the local user just added and wants opened as soon as
    /// it lands back through the document reconcile (peer-added connections do
    /// not set this, so they are listed but not auto-connected).
    pending_connect_id: Option<Uuid>,
    /// Active resource invocation plus whether its effect can be retried
    /// safely without duplicating a database write.
    /// The caller of a running "Run SQL" effect, answered when the query
    /// settles.
    effect_reply: Option<EffectReply>,
    /// The record as last presented.
    record: Rc<Record>,
    export_rx: Option<ui::portal::PortalChannel>,
    export_snapshot: Option<crate::model::QueryResult>,
    /// The dialog over the window, if any.
    dialog: Option<Dialog>,
    /// Potentially destructive SQL awaiting an explicit second confirmation.
    pending_query_confirmation: Option<String>,
    /// Retained, revisioned global-search projection. It is rebuilt only when
    /// the live catalog or collaborative query collection changes.
    search_index: crate::search::DbSearchIndex,
    published: Published,
}

impl Default for DbClientApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DbClientApp {
    /// The app opens on the workspace's saved connections (empty on a fresh
    /// record) and never fabricates a sample database or profiles.
    pub fn new() -> Self {
        Self::build(Session::new())
    }

    /// A client on the bundled sample catalog, connected, beside two
    /// remotes: the populated widget the tests drive.
    #[cfg(test)]
    pub(crate) fn sampled() -> Self {
        Self::build(Session::with_sample())
    }

    fn build(session: Session) -> Self {
        let link = Link::new();
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let actions: Vec<ui::widget::Element> = vec![Box::new(search)];
        let shell = NavShell::new()
            .with_id("db-client")
            .sidebar(Box::new(Navigator::new(&link)))
            .with_filter(ui::widget::SearchField::filter().with_placeholder("Filter tables"))
            .content(Box::new(Workspace::new(&link)))
            .content_header("", actions);
        let mut app = Self {
            shell,
            link,
            session,
            actions: None,
            secrets: None,
            pending_connect_id: None,
            effect_reply: None,
            record: Rc::default(),
            export_rx: None,
            export_snapshot: None,
            dialog: None,
            pending_query_confirmation: None,
            search_index: crate::search::DbSearchIndex::default(),
            published: Published::default(),
        };
        app.publish();
        app
    }

    /// Stage a capture. Every populated scenario opens the connection that
    /// stands for the bundled sample catalog and runs the query the
    /// workspace holds against it, then shows what it names: `data` and
    /// `structure` a table's rows and columns, `wide` the track listing,
    /// `connect` the new-connection form, `confirm` the confirmation before
    /// a statement that changes data, and `error` a remote that could not be
    /// reached. `empty` is the first run.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("empty") {
            return;
        }
        if let Some(idx) = self.session.sample_index() {
            self.connect(idx);
            self.session.block_until_idle();
            self.session.drain();
        }
        if self.session.connected {
            self.show_query_result(scenario_query(scenario));
        }
        match scenario {
            Some("data") => {
                self.open_table("albums");
                self.show_tab(MainTab::Data);
                self.session.block_until_idle();
                self.session.drain();
            }
            Some("structure") => {
                self.open_table("tracks");
                self.show_tab(MainTab::Structure);
            }
            Some("connect") => self.open_connect_dialog(),
            Some("confirm") => self.confirm_statement("DROP TABLE albums;"),
            Some("error") => self.fail_connecting(
                "Production analytics",
                "Connection refused: the server at db.internal:5432 did not answer.",
            ),
            _ => {}
        }
        self.publish();
    }

    /// The window shell, for tests and shots.
    pub fn shell(&self) -> &NavShell {
        &self.shell
    }

    pub fn shell_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    /// Show a tab, loading what it needs.
    pub fn show_tab(&mut self, tab: MainTab) {
        self.session.tab = tab;
        if tab == MainTab::Data {
            self.session.load_table_data();
        }
    }

    /// Open a table the way a click on it in the sidebar does.
    pub fn open_table(&mut self, name: &str) {
        self.session.select_table(name);
        self.session.block_until_idle();
        self.session.drain();
    }

    /// Run `sql` on the open connection and wait for its result.
    pub fn show_query_result(&mut self, sql: &str) {
        self.session.run_query(sql);
        self.session.block_until_idle();
        self.session.drain();
    }

    /// Open the new-connection dialog.
    pub fn open_connect_dialog(&mut self) {
        self.dialog = Some(Dialog::Connect);
    }

    /// Ask before running `sql`, the way a statement that changes data does.
    pub fn confirm_statement(&mut self, sql: &str) {
        self.pending_query_confirmation = Some(sql.to_owned());
        self.dialog = Some(Dialog::ConfirmRun);
    }

    /// Fail the session with `message`, for staging an error.
    pub fn fail_with(&mut self, message: &str) {
        self.session.status = RunStatus::Error(message.to_owned());
    }

    /// Stage an attempt to open the connection called `name` that ended in
    /// `message`: that connection is the active one, and it is not open.
    pub fn fail_connecting(&mut self, name: &str, message: &str) {
        self.session.fail_connecting(name, message);
    }

    /// Run the statement in the editor, after asking when it changes data.
    fn run_statement(&mut self, sql: String) {
        if sql_is_potentially_destructive(&sql) {
            self.pending_query_confirmation = Some(sql);
            self.dialog = Some(Dialog::ConfirmRun);
            return;
        }
        self.run_sql_effect(sql);
    }

    fn run_sql_effect(&mut self, sql: String) {
        // The toolbar runs the very effect the palette and agents invoke.
        if let Err(error) = self.execute_effect(DbEffect::Query { sql }) {
            self.session.status = RunStatus::Error(error);
        }
    }

    /// Do what a view asked.
    fn apply(&mut self, intent: Intent) {
        match intent {
            Intent::Connect(id) => {
                // Use the shared effect implementation for both selecting and
                // retrying an already-active disconnected profile.
                let _ = self.execute_effect(DbEffect::Connect {
                    connection: id_of(id).as_str().to_owned(),
                });
            }
            Intent::OpenItem(id) => self.open_item(id),
            Intent::RemoveConnection(id) => {
                if let Some(index) = self.session.connections.iter().position(|c| c.id == id) {
                    self.remove_connection(index);
                }
            }
            Intent::NewConnection => self.dialog = Some(Dialog::Connect),
            Intent::SaveConnection(profile) => {
                self.save_connection(*profile);
                self.dialog = None;
            }
            Intent::OpenRelation(relation) => {
                self.session.select_relation(relation);
                if self.session.tab == MainTab::Query {
                    self.session.tab = MainTab::Data;
                }
            }
            Intent::SelectTab(tab) => self.show_tab(tab),
            Intent::Run(sql) => self.run_statement(sql),
            Intent::ConfirmRun => {
                self.dialog = None;
                if let Some(sql) = self.pending_query_confirmation.take() {
                    self.run_sql_effect(sql);
                }
            }
            Intent::CloseDialog => {
                self.dialog = None;
                self.pending_query_confirmation = None;
            }
            Intent::LoadSnippet(index) => {
                if let Some((_, sql)) = crate::sample::starter_queries().get(index) {
                    self.replace_editor_sql(sql);
                }
            }
            Intent::DismissStatus => self.session.status = RunStatus::Idle,
            Intent::PreviousPage => self.session.prev_data_page(),
            Intent::NextPage => self.session.next_data_page(),
        }
    }

    /// Describe the session to the views.
    fn publish(&mut self) {
        let session = &self.session;
        let signature = schema_signature(&session.schema);
        if self.published.schema.as_ref().map(|(s, _)| *s) != Some(signature) {
            self.published.schema_revision = self.published.schema_revision.wrapping_add(1);
            self.published.schema = Some((signature, Rc::new(session.schema.clone())));
        }
        let schema = self
            .published
            .schema
            .as_ref()
            .map_or_else(Default::default, |(_, schema)| Rc::clone(schema));
        let revision = self.published.schema_revision;
        if self.published.query.as_ref().map(|(g, _)| *g) != Some(session.query_gen) {
            self.published.query = Some((
                session.query_gen,
                session.query_result.clone().map(Arc::new),
            ));
        }
        if self.published.data.as_ref().map(|(g, _)| *g) != Some(session.data_gen) {
            self.published.data =
                Some((session.data_gen, session.data_result.clone().map(Arc::new)));
        }
        let table = session.selected_relation.as_ref().and_then(|relation| {
            let cached = self
                .published
                .table
                .as_ref()
                .filter(|(r, rev, _)| r == relation && *rev == revision);
            if let Some((_, _, table)) = cached {
                return Some(Rc::clone(table));
            }
            let info = Rc::new(session.selected_table_info()?.clone());
            self.published.table = Some((relation.clone(), revision, Rc::clone(&info)));
            Some(info)
        });
        let model = ViewModel {
            connections: session
                .connections
                .iter()
                .map(|profile| ConnectionRow {
                    id: profile.id,
                    name: profile.name.clone(),
                    engine: profile.engine,
                    address: profile.address(),
                    removable: !profile.is_sample,
                })
                .collect(),
            active: session.active_connection_id(),
            connected: session.connected,
            busy: session.busy(),
            tab: session.tab,
            schema,
            schema_revision: revision,
            selected_relation: session.selected_relation.clone(),
            status: session.status.clone(),
            query: self.published.query.as_ref().and_then(|(_, r)| r.clone()),
            query_generation: session.query_gen,
            data: self.published.data.as_ref().and_then(|(_, r)| r.clone()),
            data_generation: session.data_gen,
            data_total: session.selected_table_rows(),
            data_offset: session.data_offset,
            data_page_size: session.data_page_size,
            table,
            dialog: self.dialog,
            pending_sql: self.pending_query_confirmation.clone().unwrap_or_default(),
            active_name: session
                .active_connection()
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
        };
        self.link.publish(model);
    }

    /// Execute a transient database effect after re-resolving every target
    /// against the current session. Resource descriptors are only a helpful
    /// snapshot: a collaborator can remove/rename a connection before an
    /// invocation arrives, so their payload is never authority by itself.
    fn focus_catalog(&mut self, connection_id: &str, schema: Option<&str>) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        if self.session.active_connection_id() != Some(id) {
            return Err("the catalog connection is no longer active".to_string());
        }
        if let Some(schema) = schema
            && !self
                .session
                .schema
                .schemas
                .iter()
                .any(|candidate| candidate.name == schema)
        {
            return Err("schema is no longer available in the active catalog".into());
        }
        self.shell.open_sidebar();
        Ok(())
    }

    fn show_result(&mut self, connection_id: &str, generation: usize) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        if self.session.active_connection_id() != Some(id)
            || self.session.query_gen != generation
            || self.session.query_result.is_none()
        {
            return Err("the query result is no longer current".into());
        }
        self.session.tab = MainTab::Query;
        Ok(())
    }

    fn run_history(&mut self, connection_id: &str, generation: usize) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        let sql = self
            .session
            .query_history
            .iter()
            .find(|entry| entry.connection_id == id && entry.generation == generation)
            .map(|entry| entry.sql.clone())
            .ok_or_else(|| "the local history entry is no longer available".to_string())?;
        if self.session.active_connection_id() != Some(id) || !self.session.connected {
            return Err("the history entry's connection is no longer open".into());
        }
        self.session.run_query(&sql);
        Ok(())
    }

    fn drain_export_portal(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.export_rx = None;
                let Some(result) = self.export_snapshot.take() else {
                    return;
                };
                match write_csv(&path, &result) {
                    Ok(()) => {
                        self.session.status = RunStatus::Ok(format!(
                            "Exported {} rows to {}",
                            result.rows.len(),
                            path.display()
                        ));
                    }
                    Err(error) => {
                        self.session.status =
                            RunStatus::Error(format!("CSV export failed: {error}"));
                    }
                }
            }
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status = RunStatus::Idle;
            }
            Ok(ui::portal::PortalOutcome::Failed(error)) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status =
                    RunStatus::Error(format!("Couldn't open the save dialog: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status =
                    RunStatus::Error("CSV save dialog closed unexpectedly".into());
            }
        }
    }

    /// Replace the shared SQL buffer with `sql`: one field edit, written as
    /// the smallest splice so a collaborator's concurrent typing survives.
    /// For a participant who may only view, the buffer stays as it is; the
    /// editor shows the stored text again on its next sync.
    fn replace_editor_sql(&self, sql: &str) {
        self.submit(Record::paths().sql.replace(sql));
    }

    /// Submit a field edit or a verb of the shared record.
    fn submit(&self, submit: impl Submit<Record>) {
        if let Some(actions) = &self.actions {
            actions.submit(submit);
        }
    }

    /// What opening an item does — a search hit, an agent's `open`: a saved
    /// connection opens, a saved query is loaded into the editor. An id that
    /// names neither is refused.
    /// The catalogue's search index, shared with the opener of its hits.
    pub(crate) fn search_index(&self) -> crate::search::DbSearchIndex {
        self.search_index.clone()
    }

    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&ui::tilo_resource::Id) -> Result<(), ui::tilo_resource::Refusal> + 'static
    {
        let link = self.link.clone();
        move |id| {
            link.ask(Intent::OpenItem(uuid_of(id)));
            Ok(())
        }
    }

    /// Open the item `id`: connect a saved connection, load a saved query.
    fn open_item(&mut self, id: Uuid) {
        if let Some(index) = self.session.connections.iter().position(|c| c.id == id) {
            self.connect(index);
        } else if let Some(sql) = self.record.saved_query(id).map(|saved| saved.sql.clone()) {
            self.replace_editor_sql(&sql);
            self.show_tab(MainTab::Query);
        }
    }

    /// Show the saved connections the workspace holds, and open the one the
    /// local user just added once it has landed.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<Record>) {
        self.record = Rc::clone(state);
        let connections = state.profiles();
        let same = connections.len() == self.session.connections.len()
            && connections
                .iter()
                .zip(&self.session.connections)
                .all(|(incoming, local)| {
                    incoming.id == local.id
                        && incoming.name == local.name
                        && incoming.engine == local.engine
                        && incoming.host == local.host
                        && incoming.port == local.port
                        && incoming.database == local.database
                        && incoming.user == local.user
                        && incoming.file_path == local.file_path
                });
        if !same {
            self.session.set_connections(connections);
        }
        if let Some(id) = self.pending_connect_id
            && let Some(idx) = self.session.connections.iter().position(|c| c.id == id)
        {
            self.pending_connect_id = None;
            self.connect(idx);
        }
        self.publish();
    }

    /// Attach the framework dispatcher, this device's keyring, and in a
    /// capture the sample catalog that stands in for a database server.
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Record>) {
        let dispatcher = cx.dispatcher();
        self.link.attach_sql_edits(dispatcher.clone());
        self.actions = Some(dispatcher);
        // A capture or a past version reaches no keyring.
        if cx.mode().is_live() {
            self.secrets = Some(cx.secrets());
        }
        if let Some(sample) = cx.fixture::<crate::resource::SampleDatabase>() {
            self.session.open_sample_as(sample.connection);
        }
    }

    /// Open the saved connection at `idx`, with the password this device
    /// keeps for it.
    fn connect(&mut self, idx: usize) {
        let Some(id) = self.session.connections.get(idx).map(|profile| profile.id) else {
            return;
        };
        if !self.session.knows_password(id)
            && let Some(secret) = self.password_secret(id)
        {
            match secret.get_blocking() {
                Ok(Some(password)) => self
                    .session
                    .remember_password(id, String::from_utf8_lossy(password.expose()).into_owned()),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "reading a connection password failed"),
            }
        }
        // Reconnect even when the requested profile is already the selected
        // row, so an active-but-disconnected profile can be retried.
        self.session.active = idx;
        self.session.connect_active();
    }

    /// The keyring entry of the password of saved connection `id`.
    fn password_secret(&self, id: Uuid) -> Option<ui::secrets::ItemSecret> {
        self.secrets
            .as_ref()
            .map(|secrets| secrets.item(PASSWORD_PURPOSE, &id.to_string()))
    }

    pub(crate) fn session_connected(&self) -> bool {
        self.session.connected
    }

    /// The live entities the client shows that the workspace does not hold:
    /// an in-flight connect / query / export job, the current query result,
    /// and the open connection's catalog, transactions and query history.
    pub(crate) fn live_rows(&self) -> Vec<LiveRow> {
        let mut rows = Vec::new();
        if let Some(label) = self.live_job_label() {
            rows.push(LiveRow::new("database.job", "database.job", label));
        }
        rows.extend(self.search_index.rows().into_iter().map(live_row));
        rows
    }

    /// Every row of the open connection's catalog as a search entry.
    pub(crate) fn catalog_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.search_index.entries()
    }

    /// The live entity `id`, at any depth of the open connection's catalog.
    pub(crate) fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.search_index.resource(id).map(live_row)
    }

    /// In-flight connect / query / export job label.
    fn live_job_label(&self) -> Option<String> {
        match &self.session.status {
            RunStatus::Busy(label) => Some(label.clone()),
            _ if self.effect_reply.is_some() => Some("Database operation…".into()),
            _ => None,
        }
    }

    /// Save a new connection profile through the action model — the metadata
    /// lands in `root["connections"]` (collaborative, undoable) while the
    /// password goes to this device's keyring — and open it once it lands.
    fn save_connection(&mut self, profile: crate::model::ConnectionProfile) {
        let id = profile.id;
        let at = self.record.connections.len();
        self.submit(Record::paths().connections.insert_as(
            &id_of(id),
            at,
            &SavedConnection::of(&profile),
        ));
        if let Some(secret) = self.password_secret(id) {
            let stored = if profile.password.is_empty() {
                secret.delete_blocking()
            } else {
                secret.put_blocking(profile.password.as_bytes())
            };
            if let Err(error) = stored {
                tracing::warn!(%error, "keeping the connection password failed");
            }
        }
        self.session.remember_password(id, profile.password);
        self.pending_connect_id = Some(id);
    }

    /// Remove a saved connection by its index in the sidebar list, and the
    /// password this device kept for it.
    fn remove_connection(&self, idx: usize) {
        let Some(profile) = self.session.connections.get(idx).cloned() else {
            return;
        };
        if let Some(connection) = self.record.connection_id(profile.id) {
            self.submit(Record::REMOVE_CONNECTION.with(connection));
        }
        if let Some(secret) = self.password_secret(profile.id)
            && let Err(error) = secret.delete_blocking()
        {
            tracing::warn!(%error, "forgetting the connection password failed");
        }
    }
}

/// The query a staged capture shows the result of: the track listing for
/// `wide`, the starter query otherwise.
pub(crate) fn scenario_query(scenario: Option<&str>) -> &'static str {
    if scenario == Some("wide") {
        crate::sample::starter_queries()
            .iter()
            .find(|(name, _)| *name == "Track listing")
            .map_or(crate::sample::DEFAULT_QUERY, |(_, sql)| *sql)
    } else {
        crate::sample::DEFAULT_QUERY
    }
}

/// Conservative editor-side safety check. It deliberately asks for review on
/// the common data-changing verbs, while harmless `SELECT`/`EXPLAIN` keeps the
/// one-keystroke run flow. The worker remains the source of truth for actual
/// SQL validity and permissions.
#[allow(
    clippy::while_let_on_iterator,
    reason = "the lexer must peek and consume paired delimiters"
)]
fn sql_is_potentially_destructive(sql: &str) -> bool {
    let mut code = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek() == Some(&'-') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\n' {
                    code.push(' ');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    code.push(' ');
                    break;
                }
            }
        } else if ch == '\'' || ch == '"' || ch == '`' {
            let quote = ch;
            while let Some(c) = chars.next() {
                if c == quote {
                    if chars.peek() == Some(&quote) {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            code.push(' ');
        } else {
            code.push(ch);
        }
    }
    let statements = code
        .split(';')
        .filter(|part| !part.trim().is_empty())
        .count();
    if statements != 1 {
        return true;
    }
    let words = code
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>();
    let Some(first) = words.first() else {
        return true;
    };
    if first == "WITH" {
        return true;
    }
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "ALTER"
                | "CREATE"
                | "DELETE"
                | "DROP"
                | "INSERT"
                | "REPLACE"
                | "TRUNCATE"
                | "UPDATE"
                | "MERGE"
                | "GRANT"
                | "REVOKE"
        )
    })
}

fn write_csv(path: &std::path::Path, result: &crate::model::QueryResult) -> Result<(), String> {
    // The portal owns naming/overwrite UX, but never rely on that as write
    // authority: refuse a raced or pre-existing destination rather than
    // silently replacing somebody else's export.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let mut write_row = |cells: &[String]| -> Result<(), String> {
        let row = cells
            .iter()
            .map(|cell| csv_field(cell))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(file, "{row}").map_err(|error| error.to_string())
    };
    write_row(&result.columns)?;
    for row in &result.rows {
        let cells = row
            .iter()
            .map(crate::model::Cell::display)
            .collect::<Vec<_>>();
        write_row(&cells)?;
    }
    file.flush().map_err(|error| error.to_string())
}

/// Make a CSV field safe to open in spreadsheet software. A leading formula
/// sigil remains data (prefixed with an apostrophe), including when hidden
/// behind whitespace/control characters; the CSV itself still quotes every
/// field so commas/newlines/quotes remain lossless.
fn csv_field(value: &str) -> String {
    let formula = value
        .trim_start_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control())
        .starts_with(['=', '+', '-', '@']);
    let safe = if formula {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}

/// Whether a key press is Ctrl (Command on macOS) with `key`.
fn is_command(event: &WindowEvent, modifiers: winit::event::Modifiers, wanted: &str) -> bool {
    let WindowEvent::KeyboardInput {
        event:
            KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
        ..
    } = event
    else {
        return false;
    };
    let state = modifiers.state();
    if !(state.control_key() || state.super_key()) {
        return false;
    }
    match logical_key {
        Key::Named(NamedKey::Enter) => wanted == "Enter",
        Key::Character(text) => text.eq_ignore_ascii_case(wanted),
        _ => false,
    }
}

impl ui::widget::Decorator for DbClientApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // Fold in any results the background worker has finished (connect
        // outcomes, query result sets, table pages). This is what makes a
        // remote query land on screen without further user input.
        self.session.drain();
        self.settle_effect();
        if self.export_rx.is_some() {
            self.drain_export_portal();
        }

        // The saved connections come from the workspace as presented
        // (`adopt_resource_state`); the widget writes nothing itself.
        for intent in self.link.take_intents() {
            self.apply(intent);
        }

        if let Err(error) = self.search_index.sync(&self.session) {
            // Preserve the last known-good catalogue for existing palette
            // entries, but make a rejected rebuild visible rather than
            // silently advertising stale discovery as current.
            self.session.status = RunStatus::Error(error);
        }

        self.publish();
        let title = self.link.read(|model| model.active_name.clone());
        self.shell.set_content_title(title);
        Widget::sync(&mut self.shell, state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The window's own shortcuts come before the focused widget sees the
        // key, unless a dialog is asking for something else.
        if self.dialog != Some(Dialog::Connect) && is_command(event, *ctx.modifiers, "Enter") {
            let intent = if self.dialog == Some(Dialog::ConfirmRun) {
                Intent::ConfirmRun
            } else {
                Intent::Run(self.link.sql())
            };
            self.link.ask(intent);
            *ctx.handled = true;
            return;
        }
        if self.dialog.is_none() && is_command(event, *ctx.modifiers, "n") {
            self.link.ask(Intent::NewConnection);
            *ctx.handled = true;
            return;
        }
        Widget::handle_event(&mut self.shell, event, ctx);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while the worker has a connect/query in flight
        // so its result lands without further input.
        self.session.busy()
            || self.effect_reply.is_some()
            || self.export_rx.is_some()
            || self.shell.needs_animation_frame()
    }

    fn name(&self) -> &'static str {
        "DbClientApp"
    }
}

impl DbClientApp {}

/// A workspace document whose SQL buffer holds the starter query, as a
/// capture stages it: what the tests show beside a sampled client.
#[cfg(test)]
pub(crate) fn sample_document() -> AutoCommit {
    let mut replica = ui::tilo_resource::Replica::<Record>::new();
    replica
        .apply(
            ui::tilo_resource::Invoke::edit(
                &Record::paths().sql.replace(crate::sample::DEFAULT_QUERY),
            )
            .expect("the starter query is an edit"),
        )
        .expect("a new record takes the starter query");
    AutoCommit::load(&replica.save()).expect("the record loads")
}

/// [`sample_document`] as a headless driver's state.
#[cfg(test)]
pub(crate) fn sample_state() -> ui::State {
    ui::State::from_doc(sample_document())
}

/// The SQL buffer as the framework hosts it: every edit a splice of the SQL
/// child, through the workspace's dispatcher.
#[cfg(test)]
mod editor_sql_tests {
    use ui::a11y::Action as A11yAction;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::entry::Did;

    use crate::model::{ConnectionProfile, Engine};
    use crate::record::{Workspace as Record, adding};
    use crate::resource::DbClientShell;

    fn sql(app: &TestApp<DbClientShell>) -> String {
        app.state().sql
    }

    /// The SQL buffer's edits in history: those naming the SQL as the field
    /// edited.
    fn sql_edits(app: &TestApp<DbClientShell>) -> usize {
        app.history()
            .into_iter()
            .filter(|context| matches!(&context.what, Some(Did::Edit(edit)) if edit.field == "sql"))
            .count()
    }

    /// A workspace with one saved connection, which shows the query editor,
    /// with the editor focused.
    fn workspace() -> TestApp<DbClientShell> {
        let mut app = TestApp::<DbClientShell>::new().unwrap();
        app.invoke(adding(&ConnectionProfile::new("Local", Engine::Sqlite), 0))
            .unwrap();
        app.harness().click_named("sql-editor").unwrap();
        app
    }

    #[test]
    fn typing_sql_is_a_splice_of_the_sql_field_undone_a_word_at_a_time() {
        let mut app = workspace();
        app.harness().type_text("select name");
        assert_eq!(sql(&app), "select name");
        assert!(
            sql_edits(&app) > 0,
            "history names the SQL as the resource edited"
        );
        assert!(app.undo());
        assert_eq!(sql(&app), "select ");
        assert!(app.undo());
        assert_eq!(sql(&app), "");
        assert_eq!(
            app.state().connections.len(),
            1,
            "undoing the typing leaves the connection"
        );
    }

    #[test]
    fn a_viewers_keystrokes_change_nothing_and_the_editor_shows_the_stored_sql() {
        let mut app = workspace();
        app.harness().type_text("select 1");
        app.set_role(ui::Role::Viewer);
        let before = app.versions().len();
        app.harness().key("End");
        app.harness().type_text(" drop table");
        app.harness()
            .perform_named("sql-editor", &A11yAction::SetValue("drop table".into()))
            .unwrap();
        assert_eq!(sql(&app), "select 1");
        assert_eq!(app.versions().len(), before);
        let inspection = app.harness().inspection_json().unwrap();
        assert!(!inspection.contains("drop table"), "{inspection}");
    }

    #[test]
    fn replacing_the_sql_is_one_undoable_edit_an_agent_can_address() {
        let mut app = workspace();
        app.harness()
            .perform_named("sql-editor", &A11yAction::SetValue("SELECT 1".into()))
            .unwrap();
        assert_eq!(sql(&app), "SELECT 1");
        app.call(
            "field-edit",
            serde_json::json!({ "edit": Record::paths().sql.replace("SELECT 2") }),
        )
        .unwrap();
        assert_eq!(sql(&app), "SELECT 2");
        let inspection = app.harness().inspection_json().unwrap();
        assert!(
            inspection.contains("SELECT 2"),
            "the editor shows it: {inspection}"
        );
        assert!(app.undo());
        assert_eq!(sql(&app), "SELECT 1");
        assert!(app.undo());
        assert_eq!(sql(&app), "");
    }

    #[test]
    fn typing_replaces_the_selection_and_merges_with_a_collaborator() {
        let mut app = workspace();
        app.harness()
            .perform_named("sql-editor", &A11yAction::SetValue("SELECT 1".into()))
            .unwrap();
        app.harness()
            .perform_named(
                "sql-editor",
                &A11yAction::SetSelection {
                    anchor: 0,
                    focus: "SELECT".len(),
                },
            )
            .unwrap();
        app.harness().type_text("WITH");
        assert_eq!(sql(&app), "WITH 1");
        app.merge_peer_edit(|doc| {
            ui::framework::testing::dispatcher::<Record>("so.tilo.db-client")
                .submit_now(
                    &mut ui::widget::Edits::lend(doc),
                    Record::paths().sql.splice(6, 0, " AS t"),
                )
                .unwrap();
        });
        app.harness().key("End");
        app.harness().type_text(";");
        assert_eq!(sql(&app), "WITH 1 AS t;");
    }

    /// Choosing a starter query replaces the editor's text with it, as one
    /// edit of the SQL child.
    #[test]
    fn choosing_a_starter_query_loads_it_into_the_editor() {
        let mut app = workspace();
        app.harness()
            .click_named("db.query.snippets")
            .expect("the starter menu is on the toolbar");
        app.harness().key("ArrowDown");
        app.harness().key("Enter");
        app.settle();
        let loaded = sql(&app);
        assert!(
            crate::sample::starter_queries()
                .iter()
                .any(|(_, starter)| *starter == loaded),
            "the menu's choice replaced the query: {loaded}"
        );
        let inspection = app.harness().inspection_json().unwrap();
        let first_line = loaded.lines().next().unwrap_or_default();
        assert!(inspection.contains(first_line), "{inspection}");
    }
}

impl DbClientApp {
    fn execute_effect(&mut self, effect: DbEffect) -> Result<(), String> {
        match effect {
            DbEffect::FocusDatabase { connection } => {
                self.focus_catalog(&connection.to_string(), None)?;
            }
            DbEffect::FocusSchema { connection, schema } => {
                self.focus_catalog(&connection.to_string(), Some(&schema))?;
            }
            DbEffect::ShowResult {
                connection,
                generation,
            } => self.show_result(&connection.to_string(), generation)?,
            DbEffect::Rerun {
                connection,
                generation,
            } => self.run_history(&connection.to_string(), generation)?,
            DbEffect::Query { sql } => self.run_on_open(&sql)?,
            DbEffect::Run { query } => {
                let sql = self
                    .record
                    .saved_query(uuid_of(&ui::tilo_resource::Id::from_raw(query)))
                    .map(|saved| saved.sql.clone())
                    .ok_or_else(|| "the saved query is no longer available".to_string())?;
                self.run_on_open(&sql)?;
            }
            DbEffect::Begin { connection } => self.run_on(connection, "BEGIN")?,
            DbEffect::Commit { connection } => self.run_on(connection, "COMMIT")?,
            DbEffect::Rollback { connection } => self.run_on(connection, "ROLLBACK")?,
            DbEffect::Connect { connection } => {
                let connection = uuid_of(&ui::tilo_resource::Id::from_raw(connection));
                let idx = self
                    .session
                    .connections
                    .iter()
                    .position(|candidate| candidate.id == connection)
                    .ok_or_else(|| "connection is no longer available".to_string())?;
                self.connect(idx);
            }
            DbEffect::Browse {
                connection,
                schema,
                table,
            } => {
                if self.session.active_connection_id() != Some(connection)
                    || !self.session.connected
                {
                    return Err("the table's connection is no longer open".to_string());
                }
                if self.session.schema.find_relation(&schema, &table).is_none() {
                    return Err("table is no longer available in the active schema".to_string());
                }
                self.session.select_relation(crate::model::RelationRef {
                    schema,
                    name: table,
                });
            }
            DbEffect::ExportCsv {
                connection,
                generation,
            } => {
                if self.session.active_connection_id() != Some(connection)
                    || self.session.query_gen != generation
                {
                    return Err("the query result is no longer current".to_string());
                }
                let result = self
                    .session
                    .query_result
                    .clone()
                    .ok_or_else(|| "there is no query result to export".to_string())?;
                if !result.is_select {
                    return Err(
                        "only row-returning query results can be exported as CSV".to_string()
                    );
                }
                if self.export_rx.is_some() {
                    return Err("an export destination is already being selected".to_string());
                }
                self.export_snapshot = Some(result);
                self.export_rx = Some(
                    ui::portal::PickRequest::save(
                        "Export query results",
                        "Export CSV",
                        "query-results.csv",
                    )
                    .filter(
                        ui::portal::FileFilter::new("CSV (comma-separated values)").glob("*.csv"),
                    )
                    .thread_name("db-client-export-csv")
                    .send(),
                );
                self.session.status = RunStatus::Busy("Choose where to save the CSV…".into());
            }
        }

        Ok(())
    }
    fn run_on_open(&mut self, sql: &str) -> Result<(), String> {
        if self.session.active_connection_id().is_none() || !self.session.connected {
            return Err("connect to a database before running SQL".to_string());
        }
        self.session.run_query(sql);
        Ok(())
    }
    fn run_on(&mut self, connection: Uuid, sql: &str) -> Result<(), String> {
        if self.session.active_connection_id() != Some(connection) || !self.session.connected {
            return Err("the requested connection is no longer open".to_string());
        }
        self.session.run_query(sql);
        Ok(())
    }
    pub(crate) fn answer_effect(
        &mut self,
        effect: DbEffect,
        cx: &mut ui::framework::EffectCx<Record, Self>,
    ) -> ui::framework::EffectAnswer {
        if matches!(
            effect,
            DbEffect::FocusDatabase { .. }
                | DbEffect::FocusSchema { .. }
                | DbEffect::ShowResult { .. }
        ) {
            self.execute_effect(effect)?;
            return Ok(ui::framework::Effected::done());
        }
        if self.effect_reply.is_some() || self.session.busy() {
            return Err("another database operation is already running".into());
        }
        self.execute_effect(effect)?;
        self.effect_reply = Some(cx.reply());

        Ok(ui::framework::Effected::Pending)
    }
    fn settle_effect(&mut self) {
        let Some(reply) = &self.effect_reply else {
            return;
        };
        if reply.is_cancelled() {
            self.session.cancel_in_flight_query();
            self.effect_reply = None;

            return;
        }
        self.drain_export_portal();
        if self.session.busy() || self.export_rx.is_some() {
            return;
        }
        if let Some(reply) = self.effect_reply.take() {
            reply.finish(match &self.session.status {
                RunStatus::Error(message) => Err(message.clone()),
                _ => Ok(ui::tilo_actions::ActionOutput::default()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::record::adding;
    use automerge::ROOT;
    use ui::headless::FrameHarness;
    use ui::theme::Theme;

    use crate::model::{ConnectionProfile, Engine};

    fn of_kind<'a>(
        resources: &'a [ui::tilo_actions::Resource],
        kind: &str,
    ) -> Option<&'a ui::tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.kind == kind {
                Some(resource)
            } else {
                of_kind(&resource.children, kind)
            }
        })
    }

    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::sampled();
        let doc = sample_document();
        Widget::sync(&mut app, &doc, &ROOT);
        (app, doc)
    }

    fn mounted(app: DbClientApp) -> FrameHarness {
        FrameHarness::for_root(sample_state(), app, (1280, 800), Theme::dark())
    }

    fn app(harness: &FrameHarness) -> &DbClientApp {
        harness.root::<DbClientApp>()
    }

    /// Put `sql` in the shared buffer, as a collaborator writing it does:
    /// without the framework dispatcher the editor only shows the text.
    fn write_sql(harness: &mut FrameHarness, sql: &str) {
        ui::framework::testing::dispatcher::<Record>("so.tilo.db-client")
            .submit_now(
                &mut ui::widget::Edits::lend(&mut harness.state_mut().doc),
                Record::paths().sql.replace(sql),
            )
            .unwrap();
        harness.relayout();
    }

    /// The first-run state, by the name the harness addresses it with.
    const EMPTY_STATE: &str = "DbClientApp[0]/Workspace[0]/Placeholder";

    /// Press `key` with Ctrl held.
    fn chord(harness: &mut FrameHarness, key: &str) {
        harness.set_modifiers(&["ctrl".to_owned()]);
        harness.key(key);
        harness.set_modifiers(&[]);
    }

    /// The app starts empty with a new-connection action opening the dialog.
    #[test]
    fn start_state_is_empty_with_a_new_connection_action() {
        // --- phase: live_app_starts_empty ---
        {
            let app = DbClientApp::new();
            assert!(
                app.session.connections.is_empty(),
                "no fabricated connections"
            );
            assert!(!app.session.connected, "no live engine on a fresh machine");
            assert_eq!(app.session.schema.table_count(), 0, "no fabricated schema");
        }

        // --- phase: the_first_run_action_opens_the_new_connection_dialog ---
        {
            let mut harness = mounted(DbClientApp::new());
            assert!(
                harness.targets().contains_key(EMPTY_STATE),
                "the empty state is on screen"
            );
            harness
                .perform_named(EMPTY_STATE, &ui::a11y::Action::Activate)
                .expect("the one action is offered");
            harness.relayout();
            assert_eq!(app(&harness).dialog, Some(Dialog::Connect));
            assert!(
                harness.targets().contains_key("db.connect-dialog"),
                "the dialog is in the window"
            );
        }

        // --- phase: ctrl_n_opens_the_dialog_from_anywhere ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            chord(&mut harness, "n");
            harness.relayout();
            assert_eq!(app(&harness).dialog, Some(Dialog::Connect));
        }
    }

    /// Only a capture shows the bundled sample: its scenario lists the
    /// sample beside two remotes and stages it opened, with a result, while
    /// `empty` is the first run.
    #[test]
    fn captures_stage_the_sample_catalog_and_the_first_run() {
        use ui::framework::testing::TestApp;

        let mut capture =
            TestApp::<crate::resource::DbClientShell>::capture(Some("connect")).unwrap();
        let state = capture.state();
        assert_eq!(state.connections.len(), 3, "the sample beside two remotes");
        assert_eq!(state.sql, crate::sample::DEFAULT_QUERY);
        let inspection = capture.harness().inspection_json().unwrap();
        for shown in ["albums", "New connection"] {
            assert!(inspection.contains(shown), "missing {shown}: {inspection}");
        }

        let mut empty = TestApp::<crate::resource::DbClientShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().connections.is_empty());
        let inspection = empty.harness().inspection_json().unwrap();
        assert!(inspection.contains("No connections yet"), "{inspection}");
    }

    /// The tabs show their view, a table opened from the sidebar shows its rows, and the structure follows the selected table.
    #[test]
    fn tabs_and_the_sidebar_choose_what_is_shown() {
        let mut harness = mounted(DbClientApp::sampled());
        harness.relayout();
        assert_eq!(app(&harness).session.tab, MainTab::Query);

        // --- phase: a_tab_click_shows_that_tab ---
        {
            let rect = harness.targets()["db.tabs"];
            // The strip is split equally; the second of three segments is Data.
            harness.click_at(rect.x0 + rect.width() / 2.0, rect.center().y);
            harness.relayout();
            assert_eq!(app(&harness).session.tab, MainTab::Data);
            assert!(
                harness.wait_until(tilo_test_env::hang_bound(), |harness| {
                    let targets = harness.targets();
                    targets.contains_key("db.data.table")
                        || targets.contains_key("db.data.previous")
                }),
                "the selected Data tab finishes loading its rows"
            );
            let targets = harness.targets();
            assert!(
                targets.contains_key("db.data.table") || targets.contains_key("db.data.previous")
            );
            assert!(
                !targets.contains_key("sql-editor"),
                "the Query tab's editor leaves the tree"
            );
        }

        // --- phase: opening_a_table_loads_its_rows_and_structure ---
        {
            let root = harness.root_mut::<DbClientApp>();
            root.link
                .ask(Intent::OpenRelation(crate::model::RelationRef {
                    schema: "main".into(),
                    name: "tracks".into(),
                }));
            root.session.block_until_idle();
            harness.relayout();
            harness.root_mut::<DbClientApp>().session.block_until_idle();
            harness.relayout();
            let root = app(&harness);
            let data = root
                .session
                .data_result
                .as_ref()
                .expect("the table's rows were fetched");
            assert_eq!(data.rows.len(), 15);
            assert_eq!(
                root.session
                    .selected_relation
                    .as_ref()
                    .map(|r| r.name.as_str()),
                Some("tracks")
            );
        }
    }

    /// Without the framework dispatcher the editor only shows the shared
    /// text: typing changes nothing, and a collaborator's edit shows.
    #[test]
    fn the_editor_shows_the_shared_sql() {
        // --- phase: without_a_dispatcher_typing_changes_nothing ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            harness.state_mut().doc.commit();
            let before = harness.state_mut().doc.get_heads();
            harness.type_text("Z");
            harness.relayout();
            assert_eq!(
                harness.state_mut().doc.get_heads(),
                before,
                "the document is unchanged"
            );
            assert_eq!(
                app(&harness).link.sql(),
                <Record as ui::tilo_resource::Resource>::read(&harness.state().doc)
                    .unwrap()
                    .sql
            );
        }

        // --- phase: sync_pulls_a_collaborators_sql_edit_into_the_editor ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            write_sql(&mut harness, "SELECT * FROM remote_table");
            assert_eq!(app(&harness).link.sql(), "SELECT * FROM remote_table");
        }
    }

    /// Running a query writes nothing to the document, and a statement that changes data asks first.
    #[test]
    fn running_asks_before_changing_data_and_never_writes_the_document() {
        // --- phase: running_a_query_writes_nothing_to_the_document ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            harness.state_mut().doc.commit();
            let before = harness.state_mut().doc.get_heads();
            chord(&mut harness, "Enter");
            harness.relayout();
            assert_eq!(
                harness.state_mut().doc.get_heads(),
                before,
                "query execution is a driver side effect, never a document mutation"
            );
            assert_eq!(app(&harness).dialog, None, "a read runs without asking");
        }

        // --- phase: the_run_button_runs_the_query_in_the_editor ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            write_sql(&mut harness, "SELECT 42 AS answer");
            harness
                .click_named("db.query.run")
                .expect("Run is on the toolbar");
            harness.relayout();
            harness.root_mut::<DbClientApp>().session.block_until_idle();
            harness.relayout();
            let result = app(&harness)
                .session
                .query_result
                .as_ref()
                .expect("the query ran")
                .clone();
            assert_eq!(result.columns, ["answer"]);
            assert_eq!(result.rows[0][0].display(), "42");
        }

        // --- phase: destructive_sql_requires_an_explicit_confirmation ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            write_sql(&mut harness, "DELETE FROM tracks");
            chord(&mut harness, "Enter");
            harness.relayout();
            let root = app(&harness);
            assert_eq!(root.dialog, Some(Dialog::ConfirmRun));
            assert_eq!(
                root.pending_query_confirmation.as_deref(),
                Some("DELETE FROM tracks"),
                "the first run asks for confirmation instead of issuing the effect"
            );
            assert!(!root.session.busy());
            assert!(harness.targets().contains_key("db.confirm-dialog"));

            // Escape declines.
            harness.key("Escape");
            harness.relayout();
            let root = app(&harness);
            assert_eq!(root.dialog, None);
            assert_eq!(root.pending_query_confirmation, None);
        }

        // --- phase: confirming_runs_the_statement ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            write_sql(&mut harness, "DELETE FROM tracks WHERE track_id = 1");
            chord(&mut harness, "Enter");
            harness.relayout();
            chord(&mut harness, "Enter");
            harness.root_mut::<DbClientApp>().session.block_until_idle();
            harness.relayout();
            let root = app(&harness);
            assert_eq!(root.dialog, None);
            let result = root.session.query_result.as_ref().expect("a result landed");
            assert_eq!(result.rows_affected, Some(1));
        }
    }

    /// Saving a connection from the dialog keeps its password in this
    /// device's keyring, never in the document, and opens it once it lands;
    /// removing it forgets the password.
    #[test]
    fn a_saved_connections_password_stays_on_the_device() {
        let mut app = DbClientApp::new();
        let secrets = ui::secrets::SecretStore::memory("so.tilo.db-client");
        app.secrets = Some(secrets.clone());
        let mut doc = AutoCommit::new();
        doc.commit();
        let heads = doc.get_heads();
        Widget::sync(&mut app, &doc, &ROOT);

        let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
        profile.host = "127.0.0.1".into();
        profile.port = 1;
        profile.password = "hunter2".into();
        app.link.ask(Intent::SaveConnection(Box::new(profile)));
        Widget::sync(&mut app, &doc, &ROOT);
        assert_eq!(app.dialog, None, "saving closes the dialog");
        let id = app
            .pending_connect_id
            .expect("the client opens it once it lands");
        let secret = secrets.item(PASSWORD_PURPOSE, &id.to_string());
        assert_eq!(
            secret.get_blocking().unwrap().expect("kept").expose(),
            b"hunter2"
        );
        assert_eq!(
            doc.get_heads(),
            heads,
            "the widget writes nothing itself; its edits go through the dispatcher"
        );

        let mut listed = ConnectionProfile::new("Prod", Engine::Postgres);
        listed.id = id;
        let mut workspace = Record::default();
        workspace.connections.push(
            crate::record::id_of(id),
            crate::record::SavedConnection::of(&listed),
        );
        app.adopt_resource_state(&Rc::new(workspace));
        assert!(app.pending_connect_id.is_none(), "it opened once listed");
        assert_eq!(
            app.link.read(|model| model.connections.len()),
            1,
            "and the views are told about it"
        );
        app.remove_connection(0);
        assert!(
            !secret.present_blocking(),
            "removing it forgets the password"
        );
    }

    /// Adding a connection is one undoable step of the workspace, and the
    /// document never holds a password.
    #[test]
    fn an_added_connection_is_one_undo_step_without_a_password() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
        profile.password = "hunter2".into();
        app.invoke(adding(&profile, 0)).unwrap();
        assert_eq!(app.state().connections.len(), 1);
        let saved = app.read_document(|doc| doc.clone().save());
        assert!(!saved.windows(7).any(|window| window == b"hunter2"));
        assert!(app.undo());
        assert!(app.state().connections.is_empty());
    }

    /// A connection a collaborator added is listed but not opened: opening
    /// it is this device's choice, with this device's password.
    #[test]
    fn a_collaborators_connection_is_listed_without_being_opened() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        let profile = ConnectionProfile::new("Team DB", Engine::MySql);
        app.merge_peer_edit(|doc| {
            ui::framework::testing::dispatcher::<Record>("so.tilo.db-client")
                .submit_now(&mut ui::widget::Edits::lend(doc), adding(&profile, 0))
                .unwrap();
        });
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Team DB"), "{inspection}");
        assert!(
            !inspection.contains("Connecting to Team DB"),
            "a collaborator's connection is not opened here: {inspection}"
        );
    }

    /// The new-connection dialog adds a `SQLite` file with a Unicode path to
    /// the workspace and opens it; after a reopen the effects run SQL on it
    /// without copying results into the shared document.
    #[test]
    fn the_connection_dialog_adds_a_sqlite_connection_that_opens() {
        use ui::framework::testing::TestApp;

        let _keyring = ui::secrets::scope_thread(ui::secrets::memory_backend());
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("Grün 🌍.sqlite");
        {
            let seed = rusqlite::Connection::open(&database).unwrap();
            seed.execute_batch(
                "CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES ('Grün 🌍');",
            )
            .unwrap();
        }
        // The dialog takes the file and saves the profile, to be opened once
        // the workspace lists it.
        let mut harness = mounted(DbClientApp::new());
        harness.relayout();
        harness
            .perform_named(EMPTY_STATE, &ui::a11y::Action::Activate)
            .expect("the one action is offered");
        harness.relayout();
        // The engine picker is three equal segments; SQLite is the last.
        let picker = harness.targets()["db.modal.engine"];
        harness.click_at(picker.x1 - 4.0, picker.center().y);
        harness.relayout();
        crate::connect_modal::tests::focus_field(&mut harness, "Connection name");
        harness.type_text("Local files");
        crate::connect_modal::tests::focus_field(&mut harness, "SQLite file path");
        harness.commit_text(database.to_str().unwrap());
        harness.key("Enter");
        harness.relayout();
        let root = app(&harness);
        assert_eq!(root.dialog, None, "saving closes the dialog");
        assert!(
            root.pending_connect_id.is_some(),
            "the client opens it once it lands"
        );
        drop(harness);

        // Hosted, the saved profile opens and runs SQL.
        let mut profile = ConnectionProfile::new("Local files", Engine::Sqlite);
        profile.file_path = database.to_str().unwrap().into();
        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        app.invoke(adding(&profile, 0)).unwrap();
        let state = app.state();
        assert_eq!(state.connections.len(), 1);
        assert_eq!(state.profiles()[0].name, "Local files");

        app.invoke(
            ui::tilo_resource::Invoke::edit(
                &Record::paths().sql.replace("SELECT value FROM proof"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut app = app.reopen().unwrap();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(
            inspection.contains("SELECT value FROM proof"),
            "{inspection}"
        );
        assert!(
            of_kind(&app.harness().resources(), "database.schema").is_none(),
            "saved SQL and its filename do not mean the connection is open"
        );
        let connection = state.profiles()[0].id;

        let item = ui::tilo_resource::items::item_resource_id::<Record>(
            "connections",
            &crate::record::id_of(connection),
        );
        app.call_on(&item, "connect", serde_json::json!({}))
            .unwrap();
        tilo_test_env::wait_until("the connection to open its catalog", || {
            app.settle();
            of_kind(&app.harness().resources(), "database.schema")
                .is_some_and(|schema| schema.label == "main")
        });
        let heads = app.read_document(|doc| doc.clone().get_heads());
        app.effect_confirmed(&crate::resource::DbEffect::Query {
            sql: "SELECT value FROM proof".into(),
        })
        .unwrap_or_else(|error| panic!("{error:?}"));
        tilo_test_env::wait_until("the query to answer its caller", || {
            app.settle();
            app.effect_progress().iter().any(|update| {
                matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. })
            })
        });
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Grün 🌍"), "{inspection}");
        assert!(
            app.agent_surface()
                .to_string()
                .contains("database.query-result"),
            "the result is published as a live row"
        );
        assert_eq!(
            app.read_document(|doc| doc.clone().get_heads()),
            heads,
            "running SQL copies no result into the shared document"
        );
    }

    /// Resource queries stay pending until the worker finishes; the live rows cover schema, results and transactions without secrets and stay schema-qualified.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn resource_tree_and_queries_are_schema_qualified_and_secret_free() {
        // --- phase: resource_query_is_pending_until_the_database_worker_finishes ---
        {
            use ui::framework::testing::TestApp;
            let _keyring = ui::secrets::scope_thread(ui::secrets::memory_backend());
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("pending-query.sqlite");
            rusqlite::Connection::open(&database).unwrap();
            let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
            let mut profile = ConnectionProfile::new("Query worker", Engine::Sqlite);
            profile.file_path = database.to_string_lossy().into_owned();
            app.invoke(adding(&profile, 0)).unwrap();
            let item = ui::tilo_resource::items::item_resource_id::<Record>(
                "connections",
                &crate::record::id_of(profile.id),
            );
            app.call_on(&item, "connect", serde_json::json!({}))
                .unwrap();
            tilo_test_env::wait_until("the query fixture connection to open", || {
                fn has_schema(rows: &[Resource]) -> bool {
                    rows.iter()
                        .any(|row| row.kind == "database.schema" || has_schema(&row.children))
                }
                app.settle();
                has_schema(&app.harness().resources())
            });
            // The start door returns a receipt only for an Accepted job;
            // progress contains worker updates and the eventual result.
            let invocation = app
                .start_effect(&DbEffect::Query {
                    sql: "SELECT 1".into(),
                })
                .expect("the canonical effect transport admits the database worker job");
            assert!(
                !invocation.0.is_nil(),
                "admission returns a usable invocation receipt"
            );
            tilo_test_env::wait_until("the database worker to answer this invocation", || {
                app.settle();
                app.effect_progress().iter().any(|update| {
                    matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded {
                        invocation: completed, ..
                    } if *completed == invocation)
                })
            });
        }

        // --- phase: live_rows_cover_schema_results_and_transactions_without_secrets ---
        {
            use ui::tilo_resource::Effect as _;

            let (mut app, _doc) = synced();
            app.session.connections[0].password = "never-publish-this".into();
            let rows: Vec<Resource> = app
                .live_rows()
                .into_iter()
                .map(LiveRow::into_resource)
                .collect();
            assert!(
                rows.iter().all(|row| !matches!(
                    row.kind.as_str(),
                    "database.table" | "database.view" | "database.column"
                )),
                "the published rows stay bounded; relations resolve by id"
            );
            assert!(
                rows.iter()
                    .all(|row| !row.label.contains("never-publish-this")
                        && !row.id.as_str().contains("never-publish-this")),
                "no row carries a password"
            );
            let connection = app.session.active_connection().unwrap().id.to_string();
            let schema = &app.session.schema.schemas[0];
            let relation = &schema.tables[0];
            let relation_id =
                catalog_resource_id("relation", &[&connection, &schema.name, &relation.name]);
            let resolved = app
                .live_row(&relation_id)
                .expect("exact relation resolves")
                .into_resource();
            assert_eq!(resolved.id.as_str(), relation_id);
            assert!(resolved.actions.iter().any(|action| action.id == "browse"));
            let run_sql = crate::resource::DbEffect::specs()
                .into_iter()
                .find(|action| action.id == "query")
                .expect("Run SQL is an effect");
            assert_eq!(
                run_sql.contract.risk,
                ui::tilo_actions::RiskLevel::Destructive
            );
            assert_eq!(
                run_sql.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm,
                "an arbitrary SQL envelope is never advertised as harmless"
            );
        }

        // --- phase: relation_resources_and_browse_payloads_remain_schema_qualified ---
        {
            let (mut app, _doc) = synced();
            let original = app.session.schema.schemas[0].tables[0].clone();
            app.session.schema.schemas.push(crate::model::SchemaInfo {
                name: "other:schema".into(),
                tables: vec![original.clone()],
            });
            app.search_index.sync(&app.session).unwrap();
            let connection = app.session.active_connection().unwrap().id.to_string();
            let first_id = catalog_resource_id(
                "relation",
                &[
                    &connection,
                    &app.session.schema.schemas[0].name,
                    &original.name,
                ],
            );
            let second_id =
                catalog_resource_id("relation", &[&connection, "other:schema", &original.name]);
            assert_ne!(first_id, second_id, "encoded ids cannot collide on colons");
            let resolved = app
                .live_row(&second_id)
                .expect("exact relation resolves")
                .into_resource();
            let browse = resolved
                .actions
                .iter()
                .find(|action| action.id == "browse")
                .unwrap();
            let encoded = browse.build_json(&serde_json::Map::new()).unwrap();
            let operation =
                <DbEffect as ui::tilo_resource::Effect>::from_payload(&encoded.payload).unwrap();
            assert!(matches!(
                operation,
                DbEffect::Browse { schema, table, .. }
                    if schema == "other:schema" && table == original.name
            ));
        }
    }

    /// Destructive SQL is recognised, read queries stay fast, and CSV fields neutralise formulas without losing values.
    #[test]
    fn sql_safety_and_csv_export_guard_destructive_or_formula_input() {
        // --- phase: sql_safety_check_leaves_read_queries_fast ---
        {
            assert!(!sql_is_potentially_destructive("SELECT * FROM tracks"));
            assert!(!sql_is_potentially_destructive("EXPLAIN SELECT 1"));
            assert!(sql_is_potentially_destructive(
                "  UPDATE tracks SET name = 'x'"
            ));
            assert!(sql_is_potentially_destructive("DROP TABLE tracks"));
            assert!(sql_is_potentially_destructive(
                "-- note\nDELETE FROM tracks"
            ));
            assert!(sql_is_potentially_destructive(
                "/* note */ WITH gone AS (DELETE FROM tracks RETURNING id) SELECT * FROM gone"
            ));
            assert!(sql_is_potentially_destructive(
                "SELECT 1; DROP TABLE tracks"
            ));
            assert!(!sql_is_potentially_destructive(
                "SELECT 'DELETE FROM tracks'"
            ));
            assert!(!sql_is_potentially_destructive("-- DELETE\nSELECT 1"));
        }

        // --- phase: csv_fields_neutralize_spreadsheet_formulas_without_losing_values ---
        {
            assert_eq!(csv_field("=SUM(A1:A2)"), "\"'=SUM(A1:A2)\"");
            assert_eq!(csv_field(" \t@cmd"), "\"' \t@cmd\"");
            assert_eq!(csv_field("plain, \"text\""), "\"plain, \"\"text\"\"\"");
            assert_eq!(csv_field("NULL"), "\"NULL\"");
            assert_eq!(csv_field("Binary, 4 bytes"), "\"Binary, 4 bytes\"");
        }
    }

    /// An error stays until it is dismissed, and the data pages follow the page controls.
    #[test]
    fn errors_and_pages() {
        // --- phase: an_error_stays_until_it_is_dismissed ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            harness
                .root_mut::<DbClientApp>()
                .fail_with("Connection refused");
            harness.relayout();
            let failed = |harness: &FrameHarness| {
                app(harness)
                    .link
                    .read(|model| matches!(model.status, RunStatus::Error(_)))
            };
            assert!(failed(&harness));
            harness
                .root_mut::<DbClientApp>()
                .link
                .ask(Intent::DismissStatus);
            harness.relayout();
            assert!(!failed(&harness), "dismissing clears the error");
        }

        // --- phase: the_page_intents_move_through_the_table ---
        {
            let mut harness = mounted(DbClientApp::sampled());
            harness.relayout();
            let root = harness.root_mut::<DbClientApp>();
            root.session.data_page_size = 5;
            root.link
                .ask(Intent::OpenRelation(crate::model::RelationRef {
                    schema: "main".into(),
                    name: "tracks".into(),
                }));
            let page = |harness: &mut FrameHarness| {
                harness.root_mut::<DbClientApp>().session.block_until_idle();
                harness.relayout();
                let model = &app(harness).link;
                model.read(|m| (m.data_offset, m.data.as_ref().map_or(0, |d| d.rows.len())))
            };
            harness.relayout();
            assert_eq!(page(&mut harness), (0, 5));
            harness.root_mut::<DbClientApp>().link.ask(Intent::NextPage);
            harness.relayout();
            assert_eq!(page(&mut harness), (5, 5));
            harness
                .root_mut::<DbClientApp>()
                .link
                .ask(Intent::PreviousPage);
            harness.relayout();
            assert_eq!(page(&mut harness), (0, 5));
        }
    }

    /// Every view of the window passes the accessibility audit: the query, the data, the structure, the first-run state and the dialogs.
    #[test]
    fn every_view_passes_the_accessibility_audit() {
        let audit = |harness: &FrameHarness, what: &str| {
            let issues = harness.accessibility_audit().issues;
            assert!(issues.is_empty(), "{what}: {issues:?}");
        };

        let mut harness = mounted(DbClientApp::sampled());
        harness.relayout();
        audit(&harness, "query");
        for (tab, what) in [(MainTab::Data, "data"), (MainTab::Structure, "structure")] {
            harness.root_mut::<DbClientApp>().show_tab(tab);
            harness.relayout();
            audit(&harness, what);
        }
        harness.root_mut::<DbClientApp>().open_connect_dialog();
        harness.relayout();
        audit(&harness, "new connection");
        harness
            .root_mut::<DbClientApp>()
            .link
            .ask(Intent::CloseDialog);
        harness
            .root_mut::<DbClientApp>()
            .confirm_statement("DROP TABLE albums;");
        harness.relayout();
        audit(&harness, "confirmation");

        let mut harness = mounted(DbClientApp::new());
        harness.relayout();
        audit(&harness, "first run");
    }

    /// The staged captures are what they claim: the track listing holds the query that produced its rows, and the failed connection is a remote one with no stale results.
    #[test]
    fn staged_captures_are_consistent() {
        // --- phase: the_track_listing_shows_its_own_query_and_rows ---
        {
            let mut sampled = DbClientApp::sampled();
            sampled.stage_scenario(Some("wide"));
            let mut harness = mounted(sampled);
            let sql = scenario_query(Some("wide"));
            write_sql(&mut harness, sql);
            harness.relayout();
            assert_eq!(app(&harness).link.sql(), sql, "the editor holds the query");
            let columns = app(&harness)
                .link
                .read(|model| model.query.as_ref().map(|result| result.columns.clone()));
            assert_eq!(
                columns.as_deref(),
                Some(
                    ["track", "artist", "album", "price", "genre"]
                        .map(String::from)
                        .as_slice()
                ),
                "the rows are the ones the query selects"
            );

            let capture =
                ui::framework::testing::TestApp::<crate::resource::DbClientShell>::capture(Some(
                    "wide",
                ))
                .unwrap();
            assert_eq!(
                capture.state().sql,
                sql,
                "the capture's workspace holds the query it shows the rows of"
            );
        }

        // --- phase: a_failed_connection_is_a_remote_one_with_nothing_open ---
        {
            let mut sampled = DbClientApp::sampled();
            sampled.stage_scenario(Some("error"));
            let mut harness = mounted(sampled);
            harness.relayout();
            let (name, connected, results, failed) = app(&harness).link.read(|model| {
                (
                    model.active_name.clone(),
                    model.connected,
                    model.query.is_some() || model.data.is_some(),
                    matches!(model.status, RunStatus::Error(_)),
                )
            });
            assert_eq!(name, "Production analytics");
            assert!(!connected, "the failed connection is not open");
            assert!(!results, "another database's rows are not shown");
            assert!(failed);
        }
    }
}
