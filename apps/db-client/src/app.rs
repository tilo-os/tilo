//! Database controller: main's authorized resource and local driver model,
//! presented by a retained navigation shell and workspace.
use crate::doc::EditorDoc;
use crate::link::{ConnectionRow, Dialog, Intent, Link, ViewModel};
use crate::model::{DatabaseSchema, QueryResult, TableInfo};
use crate::navigator::Navigator;
use crate::resource::DbEffect;
use crate::session::{MainTab, RunStatus, Session};
use crate::workspace::Workspace;
use automerge::AutoCommit;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use tilo_actions::Resource;
use ui::framework::Dispatcher;
use ui::tilo_resource::{PlainText, TextEdit};
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use uuid::Uuid;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};
const PASSWORD_PURPOSE: &str = "password";
fn live_row(resource: Resource) -> ui::framework::LiveRow {
    let mut row = ui::framework::LiveRow::new(resource.id.as_str(), resource.kind, resource.label);
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
    format!("{schema:?}").hash(&mut hasher);
    hasher.finish()
}
#[derive(Default)]
struct Published {
    schema: Option<(u64, Rc<DatabaseSchema>)>,
    schema_revision: u64,
    query: Option<(usize, Option<Arc<QueryResult>>)>,
    data: Option<(usize, Option<Arc<QueryResult>>)>,
    table: Option<(crate::model::RelationRef, u64, Rc<TableInfo>)>,
}

pub struct DbClientApp {
    shell: NavShell,
    link: Link,
    session: Session,
    actions: Option<Dispatcher<crate::resource::DbClient>>,
    secrets: Option<ui::secrets::SecretStore>,
    sql_edits: Option<Dispatcher<PlainText>>,
    pending_connect_id: Option<Uuid>,
    effect_reply: Option<ui::framework::EffectReply>,
    saved_queries: Vec<crate::doc::SavedQuery>,
    export_rx: Option<ui::portal::PortalChannel>,
    export_snapshot: Option<QueryResult>,
    dialog: Option<Dialog>,
    pending_query_confirmation: Option<String>,
    search_index: crate::search::DbSearchIndex,
    published: Published,
    dirty: bool,
}
impl Default for DbClientApp {
    fn default() -> Self {
        Self::new()
    }
}
impl DbClientApp {
    /// A live client starts empty and opens only saved, explicitly chosen connections.
    pub fn new() -> Self {
        Self::build(Session::new())
    }
    fn build(session: Session) -> Self {
        let link = Link::new();
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let actions: Vec<ui::widget::Element> = vec![Box::new(search)];
        let shell = NavShell::new()
            .with_id("db-client")
            .sidebar(Box::new(Navigator::new(&link)))
            .content(Box::new(Workspace::new(&link)))
            .content_header("", actions);
        let mut app = Self {
            shell,
            link,
            session,
            actions: None,
            secrets: None,
            sql_edits: None,
            saved_queries: Vec::new(),
            dirty: true,
            pending_connect_id: None,
            effect_reply: None,
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
    pub fn shell(&self) -> &NavShell {
        &self.shell
    }
    pub fn shell_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }
    pub fn show_tab(&mut self, tab: MainTab) {
        self.session.tab = tab;
        if tab == MainTab::Data {
            self.session.load_table_data();
        }
    }
    pub fn open_table(&mut self, name: &str) {
        self.session.select_table(name);
        self.session.block_until_idle();
        self.session.drain();
    }
    pub fn show_query_result(&mut self, sql: &str) {
        self.session.run_query(sql);
        self.session.block_until_idle();
        self.session.drain();
    }
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
    pub fn open_connect_dialog(&mut self) {
        self.dialog = Some(Dialog::Connect);
    }
    pub fn confirm_statement(&mut self, sql: &str) {
        self.pending_query_confirmation = Some(sql.to_owned());
        self.dialog = Some(Dialog::ConfirmRun);
    }
    pub fn fail_with(&mut self, message: &str) {
        self.session.status = RunStatus::Error(message.to_owned());
    }
    pub fn fail_connecting(&mut self, name: &str, message: &str) {
        self.session.fail_connecting(name, message);
    }
    fn run_statement(&mut self, sql: String) {
        if sql_is_potentially_destructive(&sql) {
            self.pending_query_confirmation = Some(sql);
            self.dialog = Some(Dialog::ConfirmRun);
            return;
        }
        self.run_sql_effect(sql);
    }
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
    fn apply(&mut self, intent: Intent) {
        match intent {
            Intent::Connect(id) => {
                // Use the shared effect implementation for both selecting and
                // retrying an already-active disconnected profile.
                if let Err(error) = self.execute_effect(DbEffect::Connect { connection: id }) {
                    self.session.status = RunStatus::Error(error);
                }
            }
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
    fn run_sql_effect(&mut self, sql: String) {
        // The toolbar runs the very effect the palette and agents invoke.
        if let Err(error) = self.execute_effect(DbEffect::Query { sql }) {
            self.session.status = RunStatus::Error(error);
        }
        self.dirty = true;
    }
    fn execute_effect(&mut self, effect: DbEffect) -> Result<(), String> {
        match effect {
            DbEffect::FocusDatabase { connection } => self.focus_catalog(connection, None)?,
            DbEffect::FocusSchema { connection, schema } => {
                self.focus_catalog(connection, Some(&schema))?;
            }
            DbEffect::ShowResult {
                connection,
                generation,
            } => self.show_result(connection, generation)?,
            DbEffect::Rerun {
                connection,
                generation,
            } => self.run_history(connection, generation)?,
            DbEffect::Query { sql } => self.run_on_open(&sql)?,
            DbEffect::Run { query } => {
                let sql = self
                    .saved_queries
                    .iter()
                    .find(|saved| saved.id == query)
                    .map(|saved| saved.sql.clone())
                    .ok_or_else(|| "the saved query is no longer available".to_string())?;
                self.run_on_open(&sql)?;
            }
            DbEffect::Begin { connection } => self.run_on(connection, "BEGIN")?,
            DbEffect::Commit { connection } => self.run_on(connection, "COMMIT")?,
            DbEffect::Rollback { connection } => self.run_on(connection, "ROLLBACK")?,
            DbEffect::Connect { connection } => {
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
        self.dirty = true;
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
    fn focus_catalog(&mut self, id: Uuid, schema: Option<&str>) -> Result<(), String> {
        if self.session.active_connection_id() != Some(id) {
            return Err("the catalog connection is no longer active".to_string());
        }
        if let Some(schema) = schema {
            if !self
                .session
                .schema
                .schemas
                .iter()
                .any(|candidate| candidate.name == schema)
            {
                return Err("schema is no longer available in the active catalog".into());
            }
            self.link.reveal_schema(schema);
        }
        self.shell.open_sidebar();
        Ok(())
    }
    fn show_result(&mut self, id: Uuid, generation: usize) -> Result<(), String> {
        if self.session.active_connection_id() != Some(id)
            || self.session.query_gen != generation
            || self.session.query_result.is_none()
        {
            return Err("the query result is no longer current".into());
        }
        self.session.tab = MainTab::Query;
        Ok(())
    }
    fn run_history(&mut self, id: Uuid, generation: usize) -> Result<(), String> {
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
        self.dirty = true;
    }
    fn replace_editor_sql(&self, sql: &str) {
        if let Some(sql_edits) = &self.sql_edits {
            sql_edits.apply(&TextEdit::ReplaceText {
                text: sql.to_owned(),
            });
        }
    }
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::DbState) {
        let same =
            state.connections.len() == self.session.connections.len()
                && state.connections.iter().zip(&self.session.connections).all(
                    |(incoming, local)| {
                        incoming.id == local.id
                            && incoming.name == local.name
                            && incoming.engine == local.engine
                            && incoming.host == local.host
                            && incoming.port == local.port
                            && incoming.database == local.database
                            && incoming.user == local.user
                            && incoming.file_path == local.file_path
                    },
                );
        if !same {
            self.session.set_connections(state.connections.clone());
        }
        self.saved_queries.clone_from(&state.saved_queries);
        if let Some(id) = self.pending_connect_id
            && let Some(idx) = self.session.connections.iter().position(|c| c.id == id)
        {
            self.pending_connect_id = None;
            self.connect(idx);
        }
        self.dirty = true;
    }
    pub(crate) fn attach_framework(
        &mut self,
        cx: &ui::framework::SurfaceCx<crate::resource::DbClient>,
    ) {
        let dispatcher = cx.dispatcher();
        let sql_edits = crate::resource::sql_dispatcher(&dispatcher);
        self.link.attach_sql(sql_edits.clone());
        self.sql_edits = Some(sql_edits);
        self.actions = Some(dispatcher);
        // A capture or a past version reaches no keyring.
        if cx.mode().is_live() {
            self.secrets = Some(cx.secrets());
        }
        if let Some(sample) = cx.fixture::<crate::resource::SampleDatabase>() {
            self.session.open_sample_as(sample.connection);
        }
    }
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
        self.dirty = true;
    }
    fn password_secret(&self, id: Uuid) -> Option<ui::secrets::ItemSecret> {
        self.secrets
            .as_ref()
            .map(|secrets| secrets.item(PASSWORD_PURPOSE, &id.to_string()))
    }
    pub(crate) fn session_connected(&self) -> bool {
        self.session.connected
    }
    pub(crate) fn answer_effect(
        &mut self,
        effect: DbEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::DbClient, Self>,
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
        self.dirty = true;
        Ok(ui::framework::Effected::Pending)
    }
    fn settle_effect(&mut self) {
        let Some(reply) = &self.effect_reply else {
            return;
        };
        if reply.is_cancelled() {
            self.session.cancel_in_flight_query();
            self.effect_reply = None;
            self.dirty = true;
            return;
        }
        self.drain_export_portal();
        if self.session.busy() || self.export_rx.is_some() {
            return;
        }
        if let Some(reply) = self.effect_reply.take() {
            reply.finish(match &self.session.status {
                RunStatus::Error(message) => Err(message.clone()),
                _ => Ok(tilo_actions::ActionOutput::default()),
            });
        }
    }
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows = Vec::new();
        if let Some(label) = self.live_job_label() {
            rows.push(ui::framework::LiveRow::new(
                "database.job",
                "database.job",
                label,
            ));
        }
        rows.extend(self.search_index.rows().into_iter().map(live_row));
        rows
    }
    pub(crate) fn live_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.search_index.resource(id).map(live_row)
    }
    fn live_job_label(&self) -> Option<String> {
        match &self.session.status {
            RunStatus::Busy(label) => Some(label.clone()),
            _ if self.effect_reply.is_some() => Some("Database operation…".into()),
            _ => None,
        }
    }
    fn save_connection(&mut self, profile: crate::model::ConnectionProfile) {
        let id = profile.id;
        if let Some(actions) = &self.actions {
            actions.apply(&crate::actions::add_connection_action(&profile));
        }
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
    fn remove_connection(&self, idx: usize) {
        let Some(profile) = self.session.connections.get(idx).cloned() else {
            return;
        };
        if let Some(actions) = &self.actions {
            actions.apply(&crate::actions::remove_connection_action(profile.id));
        }
        if let Some(secret) = self.password_secret(profile.id)
            && let Err(error) = secret.delete_blocking()
        {
            tracing::warn!(%error, "forgetting the connection password failed");
        }
    }
    #[cfg(test)]
    pub(crate) fn sampled() -> Self {
        Self::build(Session::with_sample())
    }

    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.shell.snap();
        if let Some(idx) = self.session.sample_index() {
            self.connect(idx);
            self.session.block_until_idle();
            self.session.run_query(crate::sample::DEFAULT_QUERY);
            self.session.block_until_idle();
        }
        match scenario {
            Some("connect") => self.open_connect_dialog(),
            Some("confirm") => self.confirm_statement("DROP TABLE albums;"),
            Some("data") => {
                self.open_table("albums");
                self.show_tab(MainTab::Data);
            }
            Some("structure") => {
                self.open_table("tracks");
                self.show_tab(MainTab::Structure);
            }
            Some("wide") => self.run_starter_query("Track listing"),
            Some("error") => self.fail_connecting(
                "Analytics · prod",
                "Connection refused: the server at db.internal:5432 did not answer.",
            ),
            _ => {}
        }
        self.dirty = true;
        self.publish();
    }
}
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
    ui::provides!(dyn ui::search::capability::SearchSource; inner = shell);
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    type Inner = NavShell;
    fn inner(&self) -> &NavShell {
        &self.shell
    }
    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.session.drain();
        self.settle_effect();
        if self.export_rx.is_some() {
            self.drain_export_portal();
        }
        for intent in self.link.take_intents() {
            self.apply(intent);
        }
        if let Err(error) = self.search_index.sync(&self.session) {
            self.session.status = RunStatus::Error(error);
        }
        self.publish();
        self.shell
            .set_content_title(self.link.read(|model| model.active_name.clone()));
        self.shell.sync(state, path);
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

    fn needs_redraw(&self) -> bool {
        self.dirty || self.shell.needs_redraw()
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }
}
impl ui::search::capability::SearchSource for DbClientApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}
/// A workspace document whose SQL buffer holds the starter query, as a
/// capture stages it: what the layout tests show beside a sampled client.
#[cfg(test)]
pub(crate) fn sample_document() -> AutoCommit {
    use ui::tilo_resource::Resource as _;
    let mut doc = AutoCommit::new();
    crate::resource::DbClient::initialize(&mut doc).expect("a new document takes the schema");
    EditorDoc::set_sql(&mut doc, crate::sample::DEFAULT_QUERY);
    doc.commit();
    doc
}

/// [`sample_document`] as a headless driver's state.
#[cfg(test)]
pub(crate) fn sample_state() -> ui::State {
    ui::State::from_doc(sample_document())
}

#[cfg(test)]
mod editor_sql_tests {
    use ui::a11y::Action as A11yAction;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::{Invoke, PlainText, TextEdit};

    use crate::actions::add_connection_action;
    use crate::doc::EditorDoc;
    use crate::model::{ConnectionProfile, Engine};
    use crate::resource::{DbClient, DbClientShell, sql_resource_id};

    fn sql(app: &TestApp<DbClientShell>) -> String {
        app.read_document(EditorDoc::read_sql)
    }

    /// The SQL buffer's edits in history: those naming the SQL as the
    /// resource edited.
    fn sql_edits(app: &TestApp<DbClientShell>) -> usize {
        app.versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|context| {
                context
                    .resource
                    .as_ref()
                    .is_some_and(|resource| resource.id == sql_resource_id())
            })
            .count()
    }

    /// A workspace with one saved connection, which shows the query editor,
    /// with the editor focused.
    fn workspace() -> TestApp<DbClientShell> {
        let mut app = TestApp::<DbClientShell>::new().unwrap();
        app.invoke(
            Invoke::<DbClient>::apply(&add_connection_action(&ConnectionProfile::new(
                "Local",
                Engine::Sqlite,
            )))
            .unwrap(),
        )
        .unwrap();
        app.harness().click_named("sql-editor").unwrap();
        app
    }

    #[test]
    fn typing_sql_is_a_splice_of_the_sql_child_undone_a_word_at_a_time() {
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
        app.invoke_child::<PlainText>(
            &sql_resource_id(),
            Invoke::<PlainText>::apply(&TextEdit::ReplaceText {
                text: "SELECT 2".into(),
            })
            .unwrap(),
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
            let text = EditorDoc::sql_object(doc).unwrap();
            ui::automerge::transaction::Transactable::splice_text(doc, &text, 6, 0, " AS t")
                .unwrap();
        });
        app.harness().key("End");
        app.harness().type_text(";");
        assert_eq!(sql(&app), "WITH 1 AS t;");
    }

    /// A long query clamps the editor, which stays editable at its end.
    #[test]
    fn a_clamped_long_query_stays_editable_at_its_end() {
        let mut app = workspace();
        app.harness().resize((800, 480)).unwrap();
        let long_query = (0..60)
            .map(|line| format!("SELECT {line} AS value;"))
            .collect::<Vec<_>>()
            .join("\n");
        app.harness()
            .perform_named("sql-editor", &A11yAction::SetValue(long_query.clone()))
            .unwrap();
        let editor = app.harness().targets()["sql-editor"];
        assert!(editor.height() < 480.0, "the editor is clamped: {editor:?}");
        app.harness().click_at(editor.x0 + 64.0, editor.y0 + 12.0);
        app.harness()
            .perform_named(
                "sql-editor",
                &A11yAction::SetSelection {
                    anchor: long_query.len(),
                    focus: long_query.len(),
                },
            )
            .unwrap();
        app.harness().type_text(" -- edited at the end");
        assert_eq!(sql(&app), format!("{long_query} -- edited at the end"));
    }
}

#[cfg(test)]
mod retained_behavior_tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};
    use tilo_actions::Action;
    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::sampled();
        let mut doc = sample_document();
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }
    #[test]
    fn password_stays_in_the_keyring_and_removal_forgets_it() {
        let mut app = DbClientApp::new();
        let secrets = ui::secrets::SecretStore::memory("so.tilo.db-client");
        app.secrets = Some(secrets.clone());
        let mut doc = AutoCommit::new();
        doc.commit();
        let heads = doc.get_heads();
        let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
        profile.host = "127.0.0.1".into();
        profile.port = 1;
        profile.password = "hunter2".into();
        let id = profile.id;
        app.apply(Intent::SaveConnection(Box::new(profile.clone())));
        assert_eq!(app.pending_connect_id, Some(id));
        let secret = secrets.item(PASSWORD_PURPOSE, &id.to_string());
        assert_eq!(
            secret.get_blocking().unwrap().expect("kept").expose(),
            b"hunter2"
        );
        assert_eq!(
            doc.get_heads(),
            heads,
            "the controller's edits go through its dispatcher"
        );
        profile.password.clear();
        app.adopt_resource_state(&crate::resource::DbState {
            connections: vec![profile],
            ..Default::default()
        });
        assert!(app.pending_connect_id.is_none(), "it opened once listed");
        app.remove_connection(0);
        assert!(
            !secret.present_blocking(),
            "removing the profile forgets the password"
        );
    }
    #[test]
    fn running_a_query_writes_nothing_to_the_document() {
        let (mut app, mut doc) = synced();
        doc.commit();
        let heads = doc.get_heads();
        app.apply(Intent::Run("SELECT 1".into()));
        app.session.block_until_idle();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            doc.get_heads(),
            heads,
            "query execution is a driver effect, never document state"
        );
        assert!(app.session.query_result.is_some());
    }
    #[test]
    fn an_added_connection_is_one_undo_step_without_a_password() {
        use ui::framework::testing::TestApp;
        use ui::tilo_resource::Invoke;

        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
        profile.password = "hunter2".into();
        app.invoke(Invoke::apply(&crate::actions::add_connection_action(&profile)).unwrap())
            .unwrap();
        assert_eq!(app.state().connections.len(), 1);
        let saved = app.read_document(|doc| doc.clone().save());
        assert!(!saved.windows(7).any(|window| window == b"hunter2"));
        assert!(app.undo());
        assert!(app.state().connections.is_empty());
    }
    #[test]
    fn a_collaborators_connection_is_listed_without_being_opened() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        let profile = ConnectionProfile::new("Team DB", Engine::MySql);
        app.merge_peer_edit(|doc| {
            crate::actions::add_connection_action(&profile)
                .apply(doc)
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
    #[test]
    fn a_saved_connection_reopens_and_runs_sql_without_writing_results() {
        /// The first published resource of `kind`, at any depth.
        fn of_kind(resources: &[Resource], kind: &str) -> Option<Resource> {
            resources.iter().find_map(|resource| {
                (resource.kind == kind)
                    .then(|| resource.clone())
                    .or_else(|| of_kind(&resource.children, kind))
            })
        }
        use ui::framework::testing::TestApp;
        use ui::tilo_resource::{Invoke, TextEdit};

        let _keyring = ui::secrets::scope_thread(ui::secrets::memory_backend());
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("proof.sqlite");
        {
            let seed = rusqlite::Connection::open(&database).unwrap();
            seed.execute_batch(
                "CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES ('Grün 🌍');",
            )
            .unwrap();
        }
        let mut profile = ConnectionProfile::new("Local", Engine::Sqlite);
        profile.file_path = database.to_str().unwrap().into();
        let mut app = TestApp::<crate::resource::DbClientShell>::new().unwrap();
        app.invoke(Invoke::apply(&crate::actions::add_connection_action(&profile)).unwrap())
            .unwrap();
        app.invoke(
            Invoke::apply(&crate::actions::DbAction::EditSql(TextEdit::ReplaceText {
                text: "SELECT value FROM proof".into(),
            }))
            .unwrap(),
        )
        .unwrap();
        let mut app = app.reopen().unwrap();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(
            inspection.contains("SELECT value FROM proof"),
            "{inspection}"
        );

        let connection = crate::resource::connection_resource_id(profile.id);
        app.call_on(&connection, "connect", serde_json::json!({}))
            .unwrap();
        tilo_test_env::wait_until("the connection to open its catalog", || {
            app.settle();
            app.harness()
                .inspection_json()
                .is_ok_and(|inspection| inspection.contains("proof"))
        });
        let heads = app.read_document(|doc| doc.clone().get_heads());
        app.effect_confirmed(&crate::resource::DbEffect::Query {
            sql: "SELECT value FROM proof".into(),
        })
        .unwrap_or_else(|error| panic!("{error:?}"));
        tilo_test_env::wait_until("the query to answer its caller", || {
            app.settle();
            app.effect_progress()
                .iter()
                .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
        });
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

        // The result row shows itself, bound to its own generation.
        let result = of_kind(&app.harness().resources(), "database.query-result")
            .expect("the result is a live row");
        app.call_on(result.id.as_str(), "show-result", serde_json::json!({}))
            .unwrap();

        // A saved query runs from its own row, once confirmed: nobody types
        // its id or its SQL.
        let query = Uuid::new_v4();
        app.invoke(
            Invoke::apply(&crate::actions::save_query_action(
                query,
                "Proof",
                "SELECT value FROM proof",
            ))
            .unwrap(),
        )
        .unwrap();
        let saved = crate::resource::saved_query_resource_id(query);
        assert!(
            app.call_on(&saved, "run", serde_json::json!({})).is_err(),
            "running a saved query asks for confirmation"
        );
        app.effect_on_confirmed(&saved, &crate::resource::DbEffect::Run { query })
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert!(
            app.effect_progress()
                .iter()
                .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. })),
            "the saved query answers once the worker settles it"
        );
    }
    #[test]
    fn resource_tree_and_queries_are_schema_qualified_and_secret_free() {
        // --- phase: live_rows_cover_schema_results_and_transactions_without_secrets ---
        {
            use ui::tilo_resource::Effect as _;

            let (mut app, _doc) = synced();
            app.session.connections[0].password = "never-publish-this".into();
            let rows: Vec<Resource> = app
                .live_rows()
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
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
            assert_eq!(run_sql.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                run_sql.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
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
            let effect = <crate::resource::DbEffect as ui::tilo_resource::Effect>::from_payload(
                &encoded.payload,
            )
            .unwrap();
            assert!(matches!(
                effect,
                crate::resource::DbEffect::Browse { schema, table, .. }
                    if schema == "other:schema" && table == original.name
            ));
        }
    }
    #[test]
    fn sql_safety_and_csv_export_guard_destructive_or_formula_input() {
        // --- phase: destructive_sql_requires_an_explicit_editor_confirmation ---
        {
            let (mut app, _doc) = synced();
            app.run_statement("DELETE FROM tracks".into());
            assert_eq!(
                app.pending_query_confirmation.as_deref(),
                Some("DELETE FROM tracks"),
                "the first run asks for confirmation instead of issuing the effect"
            );
            assert!(!app.session.busy());
        }

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
            assert_eq!(csv_field("«4 bytes»"), "\"«4 bytes»\"");
        }
    }
    #[test]
    fn hosted_catalog_search_reaches_rows_outside_the_live_snapshot() {
        use crate::resource::DbClientShell;
        use ui::framework::testing::TestApp;
        let mut app = TestApp::<DbClientShell>::capture(None).unwrap();
        let target = app
            .harness()
            .with_widget_mut::<DbClientApp, _>(|view| {
                let template = view.session.schema.schemas[0].tables[0].clone();
                view.session.schema.schemas[0].tables = (0..1200)
                    .map(|index| {
                        let mut table = template.clone();
                        table.name = format!("catalog_table_{index:04}");
                        table.columns.clear();
                        table
                    })
                    .collect();
                view.search_index.sync(&view.session).unwrap();
                let target = "catalog_table_1199";
                assert!(
                    !view
                        .live_rows()
                        .into_iter()
                        .any(|row| row.into_resource().label == target),
                    "the target is beyond the bounded snapshot"
                );
                view.actions
                    .as_ref()
                    .unwrap()
                    .open_search(&ui::search::SearchRequest::this_app().with_query(target));
                target.to_owned()
            })
            .unwrap();
        app.settle();
        assert_eq!(app.harness().global_search_query(), Some(target.as_str()));
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label().is_some_and(|label| label.contains(&target))),
            "the hosted search includes the retained catalog provider"
        );
    }
}
