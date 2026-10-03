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

use automerge::{AutoCommit, ObjId};
use serde::{Deserialize, Serialize};
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use uuid::Uuid;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource};

use crate::doc::{ConnectionsDoc, EditorDoc};
use crate::link::{ConnectionRow, Dialog, Intent, Link, ViewModel};
use crate::model::{DatabaseSchema, QueryResult, TableInfo};
use crate::navigator::Navigator;
use crate::session::{MainTab, RunStatus, Session};
use crate::workspace::Workspace;

/// The single externally-invokable effect protocol for live database work.
/// Unlike document actions, these payloads contain only stable object ids and
/// SQL/table names — never passwords or live driver handles.
pub(crate) const DB_OPERATION_TYPE: &str = "db_client.operation";

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum DbClientOperation {
    /// Reveal a catalog location in the local navigator. It never touches the
    /// database or shared document, but it is a real view transition rather
    /// than a decorative/no-op search target.
    FocusCatalog {
        connection_id: String,
        schema: Option<String>,
    },
    /// Bring the current result back into view after resolving its generation.
    ShowResult {
        connection_id: String,
        result_generation: usize,
    },
    /// Re-run a bounded, per-device history entry. History itself remains
    /// private to a peer and is re-resolved before the external effect starts.
    RunHistory {
        connection_id: String,
        generation: usize,
    },
    RunQuery {
        connection_id: String,
        sql: String,
    },
    Connect {
        connection_id: String,
    },
    BrowseTable {
        connection_id: String,
        schema: String,
        table: String,
    },
    ExportCsv {
        connection_id: String,
        result_generation: usize,
    },
}

#[allow(
    clippy::expect_used,
    reason = "DbClientOperation contains only serializable strings, so this cannot fail without a broken serializer"
)]
pub(crate) fn encode_operation(operation: &DbClientOperation) -> EncodedAction {
    EncodedAction::encode_operation(DB_OPERATION_TYPE, operation)
        .expect("database operation payload is serializable")
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
    /// Dispatches saved-connection CRUD through the shared action model, so the
    /// connection list is collaborative, undoable document state.
    actions: crate::resource::ActionSink,
    /// Id of a connection the local user just added and wants opened as soon as
    /// it lands back through the document reconcile (peer-added connections do
    /// not set this, so they are listed but not auto-connected).
    pending_connect_id: Option<Uuid>,
    /// Active resource invocation plus whether its effect can be retried
    /// safely without duplicating a database write.
    resource_operation: Option<(tilo_actions::InvocationId, bool)>,
    export_rx: Option<ui::portal::PortalChannel>,
    export_snapshot: Option<crate::model::QueryResult>,
    /// Whether the editor buffer in the CRDT has been seeded yet.
    seeded: bool,
    /// When true this is the hermetic `--shots`/demo build: it seeds the
    /// bundled sample database and a starter query. The live app leaves this
    /// false, starts on the user's saved connections and never fabricates.
    demo: bool,
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
    /// The live app: starts on the user's saved connections (empty on a fresh
    /// machine) and never fabricates a sample database or profiles.
    pub fn new() -> Self {
        Self::build(Session::new(), false)
    }

    /// The hermetic `--shots`/demo build: the sole source of the sample
    /// database and connection profiles.
    pub fn new_demo() -> Self {
        let mut session = Session::with_sample();
        session.block_until_idle();
        Self::build(session, true)
    }

    fn build(session: Session, demo: bool) -> Self {
        let link = Link::new();
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let actions: Vec<ui::widget::Element> =
            vec![Box::new(search), Box::new(ui::widget::history_button())];
        let shell = NavShell::new()
            .with_id("db-client")
            .sidebar(Box::new(Navigator::new(&link)))
            .content(Box::new(Workspace::new(&link)))
            .content_header("", actions);
        let mut app = Self {
            shell,
            link,
            session,
            demo,
            actions: crate::resource::ActionSink::default(),
            pending_connect_id: None,
            resource_operation: None,
            export_rx: None,
            export_snapshot: None,
            seeded: false,
            dialog: None,
            pending_query_confirmation: None,
            search_index: crate::search::DbSearchIndex::default(),
            published: Published::default(),
        };
        app.publish();
        app
    }

    /// Turn a live app into the demo: the sample database and its starter
    /// query, for a `--shots` frame.
    pub fn enter_demo(&mut self) {
        self.session = Session::with_sample();
        self.session.block_until_idle();
        self.demo = true;
        self.seeded = false;
        self.published = Published::default();
        self.dialog = None;
        self.pending_query_confirmation = None;
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

    /// Load the starter query called `name` into the editor and run it, as
    /// choosing it from the menu and pressing Run does.
    pub fn run_starter_query(&mut self, name: &str) {
        let queries = crate::sample::starter_queries();
        if let Some((index, (_, sql))) = queries
            .iter()
            .enumerate()
            .find(|(_, (title, _))| *title == name)
        {
            self.link.ask(Intent::LoadSnippet(index));
            self.show_query_result(sql);
        }
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
        let Some(connection_id) = self.session.active_connection_id() else {
            self.session.status =
                RunStatus::Error("Connect to a database before running SQL".into());
            return;
        };
        // Keep the toolbar shortcut on the very same effect path exposed to
        // the command palette/agents. It carries a stable connection identity
        // and never smuggles a password into the action envelope.
        if let Err(error) = self.execute_operation(DbClientOperation::RunQuery {
            connection_id: connection_id.to_string(),
            sql,
        }) {
            self.session.status = RunStatus::Error(error);
        }
    }

    /// Do what a view asked.
    fn apply(&mut self, intent: Intent, doc: &mut AutoCommit) {
        match intent {
            Intent::Connect(id) => {
                // Use the shared effect implementation for both selecting and
                // retrying an already-active disconnected profile.
                let _ = self.execute_operation(DbClientOperation::Connect {
                    connection_id: id.to_string(),
                });
            }
            Intent::RemoveConnection(id) => {
                if let Some(index) = self.session.connections.iter().position(|c| c.id == id) {
                    self.remove_connection(index, doc);
                }
            }
            Intent::NewConnection => self.dialog = Some(Dialog::Connect),
            Intent::SaveConnection(profile) => {
                self.save_connection(*profile, doc);
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
                    self.replace_editor_sql(doc, sql);
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
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per DbClientOperation keeps resolve+dispatch together"
    )]
    fn execute_operation(&mut self, operation: DbClientOperation) -> Result<(), String> {
        match operation {
            DbClientOperation::FocusCatalog {
                connection_id,
                schema,
            } => self.focus_catalog(&connection_id, schema.as_deref())?,
            DbClientOperation::ShowResult {
                connection_id,
                result_generation,
            } => self.show_result(&connection_id, result_generation)?,
            DbClientOperation::RunHistory {
                connection_id,
                generation,
            } => self.run_history(&connection_id, generation)?,
            DbClientOperation::RunQuery { connection_id, sql } => {
                // Empty id means "active connection" — the framework resource
                // publishes Run SQL without a live session snapshot.
                let id = if connection_id.is_empty() {
                    self.session
                        .active_connection_id()
                        .ok_or_else(|| "connect to a database before running SQL".to_string())?
                } else {
                    Uuid::parse_str(&connection_id)
                        .map_err(|_| "invalid database connection id".to_string())?
                };
                if self.session.active_connection_id() != Some(id) {
                    return Err("the requested connection is no longer active".to_string());
                }
                if !self.session.connected {
                    return Err("connect to this database before running SQL".to_string());
                }
                self.session.run_query(&sql);
            }
            DbClientOperation::Connect { connection_id } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                let idx = self
                    .session
                    .connections
                    .iter()
                    .position(|connection| connection.id == id)
                    .ok_or_else(|| "connection is no longer available".to_string())?;
                // Reconnect even when the requested resource is already the
                // selected row; the old `select_connection` no-op would make
                // an active-but-disconnected profile impossible to retry.
                self.session.active = idx;
                self.session.connect_active();
            }
            DbClientOperation::BrowseTable {
                connection_id,
                schema,
                table,
            } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                if self.session.active_connection_id() != Some(id) || !self.session.connected {
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
            DbClientOperation::ExportCsv {
                connection_id,
                result_generation,
            } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                if self.session.active_connection_id() != Some(id)
                    || self.session.query_gen != result_generation
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

    /// Replace the shared SQL buffer through
    /// [`DbAction::ReplaceEditorSql`](crate::actions::DbAction::ReplaceEditorSql).
    /// Hosted sessions use the framework dispatcher; standalone uses the local host.
    fn replace_editor_sql(&mut self, doc: &mut AutoCommit, sql: &str) {
        let action = crate::actions::DbAction::ReplaceEditorSql {
            sql: sql.to_string(),
        };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
    }

    /// Show the saved connections the framework resource holds.
    /// An empty projection does not wipe a roster that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::DbState) {
        if self.demo {
            return;
        }
        if state.connections.is_empty() && !self.session.connections.is_empty() {
            return;
        }
        let same =
            state.connections.len() == self.session.connections.len()
                && state.connections.iter().zip(&self.session.connections).all(
                    |(incoming, local)| {
                        incoming.id == local.id
                            && incoming.name == local.name
                            && incoming.host == local.host
                            && incoming.database == local.database
                    },
                );
        if !same {
            self.session.set_connections(state.connections.clone());
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::DbClient>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    pub(crate) fn session_connected(&self) -> bool {
        self.session.connected
    }

    /// In-flight connect / query / export job label for hosted live extras.
    pub(crate) fn live_job_label(&self) -> Option<String> {
        match &self.session.status {
            RunStatus::Busy(label) => Some(label.clone()),
            _ if self.resource_operation.is_some() => Some("Database operation…".into()),
            _ => None,
        }
    }

    /// Current query-result resource when one is live (not document state).
    pub(crate) fn live_query_result_resource(&self) -> Option<Resource> {
        let connection_id = self.session.active_connection_id()?.to_string();
        let result = self.session.query_result.as_ref()?;
        let result_id = catalog_resource_id(
            "query-result",
            &[&connection_id, &self.session.query_gen.to_string()],
        );
        let mut resource = Resource::new(&result_id, "database.query-result", result.message());
        if let Ok(action) = ActionSpec::operation(
            "show",
            "Show query result",
            OperationKind::View,
            DB_OPERATION_TYPE,
            &DbClientOperation::ShowResult {
                connection_id: connection_id.clone(),
                result_generation: self.session.query_gen,
            },
        ) {
            resource = resource.with_action(action);
        }
        if result.is_select
            && let Ok(action) = ActionSpec::operation(
                "export_csv",
                "Export CSV",
                OperationKind::Effect,
                DB_OPERATION_TYPE,
                &DbClientOperation::ExportCsv {
                    connection_id,
                    result_generation: self.session.query_gen,
                },
            )
        {
            resource = resource.with_action(action);
        }
        Some(resource)
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Save a new connection profile. The live app routes it through the action
    /// model — the metadata lands in `root["connections"]` (collaborative,
    /// undoable) while the password is kept in memory only — and remembers to
    /// open it once it reconciles back. The demo keeps its roster as view state.
    fn save_connection(&mut self, profile: crate::model::ConnectionProfile, doc: &mut AutoCommit) {
        if self.demo {
            self.session.add_connection_blocking(profile);
            return;
        }
        let id = profile.id;
        self.session.remember_password(id, profile.password.clone());
        let action = crate::actions::add_connection_action(&profile);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
        self.pending_connect_id = Some(id);
    }

    /// Remove a saved connection by its index in the sidebar list.
    fn remove_connection(&mut self, idx: usize, doc: &mut AutoCommit) {
        let Some(profile) = self.session.connections.get(idx).cloned() else {
            return;
        };
        if self.demo {
            self.session.connections.remove(idx);
            if self.session.active >= self.session.connections.len() {
                self.session.active = self.session.connections.len().saturating_sub(1);
            }
            return;
        }
        let action = crate::actions::remove_connection_action(profile.id);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
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

    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource;
        inner = shell
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[DB_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let (pending, retryable) = self.resource_operation?;
        if pending != invocation {
            return None;
        }
        if cancellation_requested {
            if !self.session.cancel_in_flight_query() {
                return Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "db_client.cancel_not_active".into(),
                        message:
                            "The requested query already completed; no newer query was cancelled"
                                .into(),
                        details: None,
                        retryable: false,
                    },
                });
            }
            self.resource_operation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Cancellation requested for this query".into()),
            });
        }
        self.session.drain();
        self.drain_export_portal();
        if self.session.busy() || self.export_rx.is_some() {
            return None;
        }
        self.resource_operation = None;
        Some(match &self.session.status {
            RunStatus::Error(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "db_client.operation_failed".into(),
                    message: message.clone(),
                    details: None,
                    retryable,
                },
            },
            _ => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        })
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document(state, path);
        crate::doc::adopt_shared_schema(doc);
        if !self.seeded {
            // Only the demo build seeds a starter query; the live editor
            // opens empty so nothing references a database that isn't there.
            if self.demo && EditorDoc::read_sql(doc).is_empty() {
                EditorDoc::set_sql(doc, crate::sample::DEFAULT_QUERY);
            }
            self.seeded = true;
        }

        // Fold in any results the background worker has finished (connect
        // outcomes, query result sets, table pages). This is what makes a
        // remote query land on screen without further user input.
        self.session.drain();
        if self.export_rx.is_some() {
            self.drain_export_portal();
        }

        // The live app's saved-connection list is collaborative document
        // state: reconcile the session from `root["connections"]` each frame.
        // The demo keeps its seeded sample roster as view state instead.
        if !self.demo {
            let doc_conns = ConnectionsDoc::read(doc);
            self.session.set_connections(doc_conns);
            // Open a connection the local user just added, once it has landed
            // back through the reconcile.
            if let Some(id) = self.pending_connect_id
                && let Some(idx) = self.session.connections.iter().position(|c| c.id == id)
            {
                self.session.active = idx;
                self.session.connect_active();
                self.pending_connect_id = None;
            }
        }

        for intent in self.link.take_intents() {
            self.apply(intent, doc);
        }

        if let Err(error) = self
            .search_index
            .sync_for_app(&self.session, doc, !self.demo)
        {
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
        self.session.busy() || self.export_rx.is_some() || self.shell.needs_animation_frame()
    }

    fn name(&self) -> &'static str {
        "DbClientApp"
    }
}

impl ui::search::capability::SearchSource for DbClientApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::resources::PublishesResources for DbClientApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the workspace door — keep only live query results and in-flight jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        // Remote directory enumeration never walks a live schema. Deeper
        // catalog entries are reached through the retained SearchCatalog and
        // resolved by their canonical resource ids.
        let resources = if cursor.is_empty() && max_results > 0 {
            self.resources(doc)
        } else {
            Vec::new()
        };
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.is_hosted() && id == "database" {
            return None;
        }
        self.publish_resource(doc, id)
            .or_else(|| {
                self.is_hosted()
                    .then(|| {
                        crate::resource::live_extras(self)
                            .into_iter()
                            .find(|resource| resource.id.as_str() == id)
                    })
                    .flatten()
            })
            .or_else(|| self.search_index.resource(id))
    }
}

impl ui::undo::CommittedActions for DbClientApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // Hand the action model's undo bracket to the host so saved-connection
        // CRUD becomes a single undo boundary and replicates to peers.
        self.actions.take_committed()
    }
}

impl DbClientApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if !matches!(kind, OperationKind::Effect | OperationKind::View)
            || type_id != DB_OPERATION_TYPE
        {
            return Ok(false);
        }
        let operation: DbClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        self.execute_operation(operation)?;
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if !matches!(kind, OperationKind::Effect | OperationKind::View)
            || type_id != DB_OPERATION_TYPE
        {
            return Ok(None);
        }
        if kind == OperationKind::View {
            return self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                });
        }
        if self.resource_operation.is_some() || self.session.busy() {
            return Err("another database operation is already running".into());
        }
        let operation: DbClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        // Reconnecting and browsing are idempotent reads/handle opens. An
        // arbitrary SQL statement is deliberately *not* retryable: repeating
        // an INSERT/UPDATE after an uncertain network outcome could duplicate
        // a real side effect.
        let retryable = matches!(
            operation,
            DbClientOperation::Connect { .. } | DbClientOperation::BrowseTable { .. }
        );
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_operation = Some((invocation, retryable));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use automerge::ROOT;
    use tilo_actions::Action;
    use tilo_automerge::ApplyIncoming;
    use ui::headless::FrameHarness;
    use ui::theme::Theme;

    use crate::model::{ConnectionProfile, Engine};

    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::new_demo();
        let mut doc = AutoCommit::new();
        Widget::sync(&mut app, &mut doc, &ROOT);
        (app, doc)
    }

    fn mounted(app: DbClientApp) -> FrameHarness {
        FrameHarness::for_root(ui::State::new(), app, (1280, 800), Theme::dark())
    }

    fn app(harness: &FrameHarness) -> &DbClientApp {
        harness.root::<DbClientApp>()
    }

    /// The first-run state, by the name the harness addresses it with.
    const EMPTY_STATE: &str = "DbClientApp[0]/Workspace[0]/Placeholder";

    /// Press `key` with Ctrl held.
    fn chord(harness: &mut FrameHarness, key: &str) {
        harness.set_modifiers(&["ctrl".to_owned()]);
        harness.key(key);
        harness.set_modifiers(&[]);
    }

    /// The live app starts empty with a new-connection action opening the dialog; only the demo seeds the sample.
    #[test]
    fn start_state_is_empty_live_and_seeded_only_in_demo() {
        // --- phase: live_app_starts_empty ---
        {
            let app = DbClientApp::new();
            assert!(!app.demo, "live build is not the demo build");
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
            let mut harness = mounted(DbClientApp::new_demo());
            chord(&mut harness, "n");
            harness.relayout();
            assert_eq!(app(&harness).dialog, Some(Dialog::Connect));
        }

        // --- phase: demo_app_seeds_the_sample ---
        {
            let app = DbClientApp::new_demo();
            assert!(app.demo, "demo build is flagged");
            assert_eq!(app.session.connections.len(), 3, "demo seeds the roster");
            assert!(app.session.connected, "demo opens the sample engine");
            assert!(
                app.session.schema.table_count() >= 4,
                "demo has the sample schema"
            );
        }
    }

    /// The tabs show their view, a table opened from the sidebar shows its rows, and the structure follows the selected table.
    #[test]
    fn tabs_and_the_sidebar_choose_what_is_shown() {
        let mut harness = mounted(DbClientApp::new_demo());
        harness.relayout();
        assert_eq!(app(&harness).session.tab, MainTab::Query);

        // --- phase: a_tab_click_shows_that_tab ---
        {
            let rect = harness.targets()["db.tabs"];
            // The strip is split equally; the second of three segments is Data.
            harness.click_at(rect.x0 + rect.width() / 2.0, rect.center().y);
            harness.relayout();
            assert_eq!(app(&harness).session.tab, MainTab::Data);
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

    /// Typing replicates byte-identically, replaces selections in the CRDT and pulls a collaborator's edit into the editor.
    #[test]
    fn sql_editor_is_collaborative_crdt_text() {
        // --- phase: typing_replicates_byte_identically_on_a_peer ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            let before = crate::doc::EditorDoc::read_sql(&harness.state().doc);
            assert!(
                before.starts_with("SELECT"),
                "the demo seeds a starter query"
            );
            harness.state_mut().doc.commit();
            let base = harness.state_mut().doc.save();
            let mut peer = AutoCommit::load(&base).unwrap();

            harness.type_text("Z");
            harness.state_mut().doc.commit();
            let after = crate::doc::EditorDoc::read_sql(&harness.state().doc);
            assert_eq!(
                after.chars().count(),
                before.chars().count() + 1,
                "one char inserted"
            );
            peer.merge_incoming(&mut harness.state_mut().doc).unwrap();
            assert_eq!(
                harness.state_mut().doc.get_heads(),
                peer.get_heads(),
                "byte-identical convergence"
            );
        }

        // --- phase: typing_replaces_the_selected_sql_in_the_crdt ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            chord(&mut harness, "a");
            harness.type_text("WITH");
            assert_eq!(
                crate::doc::EditorDoc::read_sql(&harness.state().doc),
                "WITH",
                "selection replacement lands in the shared text"
            );
            assert_eq!(app(&harness).link.sql(), "WITH");
        }

        // --- phase: sync_pulls_a_collaborators_sql_edit_into_the_editor ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            crate::doc::EditorDoc::set_sql(
                &mut harness.state_mut().doc,
                "SELECT * FROM remote_table",
            );
            harness.relayout();
            assert_eq!(app(&harness).link.sql(), "SELECT * FROM remote_table");
        }
    }

    /// Running a query writes nothing to the document, and a statement that changes data asks first.
    #[test]
    fn running_asks_before_changing_data_and_never_writes_the_document() {
        // --- phase: running_a_query_writes_nothing_to_the_document ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
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
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            crate::doc::EditorDoc::set_sql(&mut harness.state_mut().doc, "SELECT 42 AS answer");
            harness.relayout();
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
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            chord(&mut harness, "a");
            harness.type_text("DELETE FROM tracks");
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
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            chord(&mut harness, "a");
            harness.type_text("DELETE FROM tracks WHERE track_id = 1");
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

    /// A saved connection lands in the document without its password, and a peer's connection is listed but not opened.
    #[test]
    fn connections_live_in_the_document_but_queries_do_not() {
        // --- phase: saving_a_connection_dispatches_an_action_into_the_doc ---
        {
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();
            Widget::sync(&mut app, &mut doc, &ROOT);

            let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
            profile.host = "db.internal".into();
            profile.password = "hunter2".into();
            let id = profile.id;
            app.save_connection(profile, &mut doc);

            let conns = crate::doc::ConnectionsDoc::read(&doc);
            assert_eq!(conns.len(), 1, "the connection was written to the document");
            assert_eq!(conns[0].name, "Prod");
            assert!(
                ui::undo::take_committed_action(&mut app).is_some(),
                "the action host recorded an undo boundary for the CRUD"
            );
            assert_eq!(
                app.pending_connect_id,
                Some(id),
                "the app will open it on reconcile"
            );
            let bytes = doc.save();
            assert!(
                !bytes.windows(7).any(|w| w == b"hunter2"),
                "the password must never enter the shared document"
            );
        }

        // --- phase: reconcile_surfaces_document_connections ---
        {
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();
            let p = ConnectionProfile::new("Team DB", Engine::MySql);
            crate::actions::add_connection_action(&p)
                .apply(&mut doc)
                .unwrap();

            Widget::sync(&mut app, &mut doc, &ROOT);
            assert_eq!(
                app.session.connections.len(),
                1,
                "the document connection is listed"
            );
            assert_eq!(app.session.connections[0].name, "Team DB");
            assert!(
                !app.session.connected,
                "a peer-added connection is not auto-opened"
            );
            assert_eq!(
                app.link.read(|model| model.connections.len()),
                1,
                "and the views are told about it"
            );
        }
    }

    /// The new-connection dialog adds a `SQLite` file with a Unicode path to the document and opens it; resource operations then run SQL against it.
    #[test]
    fn the_connection_dialog_adds_a_sqlite_connection_that_opens() {
        let directory = std::env::temp_dir().join(format!("tilo-db-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let database = directory.join("Grün 🌍.sqlite");
        {
            let seed = rusqlite::Connection::open(&database).unwrap();
            seed.execute_batch(
                "CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES ('Grün 🌍');",
            )
            .unwrap();
        }
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

        harness.root_mut::<DbClientApp>().session.block_until_idle();
        harness.relayout();
        let root = app(&harness);
        assert_eq!(root.dialog, None, "saving closes the dialog");
        assert!(
            root.session.connected,
            "the new connection opened: {:?} {:?}",
            root.session.connections, root.session.status
        );
        assert_eq!(root.session.connections[0].name, "Local files");
        assert_eq!(ConnectionsDoc::read(&harness.state().doc).len(), 1);

        // Resource operations resolve the connection from the document and
        // run SQL without copying the result into it.
        let saved = harness.state_mut().doc.save();
        let connection_id = app(&harness)
            .session
            .active_connection_id()
            .unwrap()
            .to_string();
        drop(harness);
        let mut doc = AutoCommit::load(&saved).unwrap();
        let mut app = DbClientApp::build(Session::new(), false);
        Widget::sync(&mut app, &mut doc, &ROOT);
        assert!(
            !app.session.connected,
            "loading metadata must not open external resources"
        );
        let heads = doc.get_heads();
        for operation in [
            DbClientOperation::Connect {
                connection_id: connection_id.clone(),
            },
            DbClientOperation::RunQuery {
                connection_id,
                sql: "SELECT value FROM proof".into(),
            },
        ] {
            let invocation = tilo_actions::InvocationId::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        DB_OPERATION_TYPE,
                        &serde_json::to_vec(&operation).unwrap()
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.session.block_until_idle();
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }
        let result = app.session.query_result.as_ref().unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0].display(), "Grün 🌍");
        assert_eq!(
            doc.get_heads(),
            heads,
            "executing SQL must not copy results into the shared document"
        );
        drop(app);
        std::fs::remove_file(&database).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    /// Resource queries stay pending until the worker finishes; the live tree covers schema, results and transactions without secrets and stays schema-qualified.
    #[test]
    fn resource_tree_and_queries_are_schema_qualified_and_secret_free() {
        // --- phase: resource_query_is_pending_until_the_database_worker_finishes ---
        {
            let (mut app, mut doc) = synced();
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&DbClientOperation::RunQuery {
                connection_id: app.session.active_connection_id().unwrap().to_string(),
                sql: "SELECT 1".into(),
            })
            .unwrap();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        DB_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.session.block_until_idle();
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // --- phase: live_resource_tree_covers_schema_results_and_transactions_without_secrets ---
        {
            let (mut app, doc) = synced();
            app.session.connections[0].password = "never-publish-this".into();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "root discovery is bounded");
            assert!(resources[0].children.is_empty());
            let connection = app.session.active_connection().unwrap().id.to_string();
            let schema = &app.session.schema.schemas[0];
            let relation = &schema.tables[0];
            let relation_id =
                catalog_resource_id("relation", &[&connection, &schema.name, &relation.name]);
            let resolved =
                ui::resources::resource(&app, &doc, &relation_id).expect("exact relation resolves");
            assert_eq!(resolved.id.as_str(), relation_id);
            assert!(resolved.actions.iter().any(|action| action.id == "browse"));
            let run_sql = resources[0]
                .actions
                .iter()
                .find(|action| action.id == "query")
                .expect("Run SQL is an exported action");
            assert_eq!(run_sql.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                run_sql.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "an arbitrary SQL envelope is never advertised as harmless"
            );
            assert!(
                ui::resources::resource(&app, &doc, "database")
                    .expect("root resolves exactly")
                    .actions
                    .iter()
                    .any(|action| action.id == "query"),
                "directory and exact root resolution publish the same actionable root"
            );
        }

        // --- phase: relation_resources_and_browse_payloads_remain_schema_qualified ---
        {
            let (mut app, mut doc) = synced();
            let original = app.session.schema.schemas[0].tables[0].clone();
            app.session.schema.schemas.push(crate::model::SchemaInfo {
                name: "other:schema".into(),
                tables: vec![original.clone()],
            });
            app.search_index.sync(&app.session, &mut doc).unwrap();
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
            let resolved =
                ui::resources::resource(&app, &doc, &second_id).expect("exact relation resolves");
            let browse = resolved
                .actions
                .iter()
                .find(|action| action.id == "browse")
                .unwrap();
            let encoded = browse.build_json(&serde_json::Map::new()).unwrap();
            let operation: DbClientOperation = serde_json::from_slice(&encoded.payload).unwrap();
            assert!(matches!(
                operation,
                DbClientOperation::BrowseTable { schema, table, .. }
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

    /// A starter query replaces the editor's text, an error stays until it is dismissed, and the data pages follow the page controls.
    #[test]
    fn starter_queries_errors_and_pages() {
        // --- phase: a_starter_query_replaces_the_editors_text ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            harness
                .root_mut::<DbClientApp>()
                .link
                .ask(Intent::LoadSnippet(1));
            harness.relayout();
            let (_, expected) = crate::sample::starter_queries()[1];
            assert_eq!(
                crate::doc::EditorDoc::read_sql(&harness.state().doc),
                expected
            );
            assert_eq!(app(&harness).link.sql(), expected);
        }

        // --- phase: choosing_from_the_starter_menu_loads_that_query ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
            harness.relayout();
            harness
                .click_named("db.query.snippets")
                .expect("the starter menu is on the toolbar");
            harness.key("ArrowDown");
            harness.key("Enter");
            harness.relayout();
            harness.relayout();
            let queries = crate::sample::starter_queries();
            let loaded = crate::doc::EditorDoc::read_sql(&harness.state().doc);
            assert!(
                queries.iter().any(|(_, sql)| *sql == loaded),
                "the menu's choice replaced the query: {loaded}"
            );
            assert_ne!(loaded, crate::sample::DEFAULT_QUERY);
        }

        // --- phase: an_error_stays_until_it_is_dismissed ---
        {
            let mut harness = mounted(DbClientApp::new_demo());
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
            let mut harness = mounted(DbClientApp::new_demo());
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

        let mut harness = mounted(DbClientApp::new_demo());
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

    /// A standalone SQL edit is submitted through the action host.
    #[test]
    fn a_standalone_sql_edit_is_submitted_through_the_action() {
        let mut app = DbClientApp::new();
        assert!(!app.actions.is_hosted());
        let mut doc = automerge::AutoCommit::new();
        app.replace_editor_sql(&mut doc, "select 1");
        assert_eq!(crate::doc::EditorDoc::read_sql(&doc), "select 1");
        let recorded = app
            .actions
            .local
            .last_dispatched()
            .expect("the SQL edit is submitted through the action sink");
        assert_eq!(recorded.type_id.as_str(), crate::actions::DB_ACTION_TYPE);
    }

    /// The staged captures are what they claim: the track listing holds the query that produced its rows, and the failed connection is a remote one with no stale results.
    #[test]
    fn staged_captures_are_consistent() {
        // --- phase: the_track_listing_shows_its_own_query_and_rows ---
        {
            let mut demo = DbClientApp::new_demo();
            demo.run_starter_query("Track listing");
            let mut harness = mounted(demo);
            harness.relayout();
            harness.relayout();
            let (_, sql) = crate::sample::starter_queries()
                .iter()
                .find(|(name, _)| *name == "Track listing")
                .expect("the starter exists");
            assert_eq!(&app(&harness).link.sql(), sql, "the editor holds the query");
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
        }

        // --- phase: a_failed_connection_is_a_remote_one_with_nothing_open ---
        {
            let mut demo = DbClientApp::new_demo();
            demo.fail_connecting("Production analytics", "Connection refused");
            let mut harness = mounted(demo);
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
