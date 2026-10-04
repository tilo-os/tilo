//! The database client's action model.
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`] — a serializable value that deterministically changes
//! the shared Automerge document, so undo and multi-peer replay come for free —
//! while presentation / view state and external side effects stay plain app
//! code. Applied to the database client that split lands as follows.
//!
//! ## Saved connections ARE collaborative document state (actions)
//!
//! The list of saved connection profiles is durable, shareable data: it is the
//! set of databases a workspace works with. It lives in the shared document at
//! `root["connections"]` (a List of profile Maps, see [`crate::doc::ConnectionsDoc`])
//! and is mutated **only** through [`DbAction`] — [`DbAction::AddConnection`],
//! [`DbAction::RemoveConnection`], [`DbAction::RenameConnection`]. Because each
//! edit is an action, adding or removing a connection is undoable and replays
//! byte-identically on an invited peer.
//!
//! The one field that never enters the document is the **password**: it is held
//! in memory for the session only ([`ConnectionProfile::password`] is
//! `#[serde(skip)]`, and the add-action builder deliberately drops it). So a
//! collaborator sees *which* databases the team uses without ever receiving a
//! secret; each peer re-enters the password before opening its own live handle.
//!
//! ## The live connection + query execution are external, NOT actions
//!
//! Opening the TCP socket, introspecting the catalog, running a query and
//! browsing a table's rows are **driver / network** side effects run on the
//! background [`crate::worker`] thread (see [`crate::engine`], [`crate::engine_pg`],
//! [`crate::engine_mysql`], [`crate::session::Session`]). They mutate a database
//! server or fill a transient in-memory [`Session`](crate::session::Session),
//! never the shared CRDT, so by the recipe's rule none of them is an action. A
//! refused socket surfaces as an honest error, not a replicated document change.
//!
//! ## The SQL buffer stays on the native text path
//!
//! The other shared-document object is the SQL query buffer, a single Automerge
//! `Text` at `root["editor"]["sql"]` (see [`crate::doc::EditorDoc`]). Flowing
//! text takes the toolkit's CRDT-native text path *instead of* the action
//! executor: the `Text` already replicates and undoes for free, so re-encoding a
//! keystroke as an action would only duplicate that. Replacing the whole
//! buffer — a snippet or an accessibility set-value — is
//! [`DbAction::ReplaceEditorSql`], which splices that same `Text`.
//!
//! ## Presentation is never an action
//!
//! Which connection is active, which table is selected, the current tab, the
//! table sort order and scroll position and whether the sidebar is shown are per-peer view state on
//! the views — presentation the recipe says must never be an action.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use automerge::AutoCommit;
use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    ExecutionMode, OperationKind, Resource, ResultSpec, RetryPolicy,
};
use ui::doc::put_if_changed;

use crate::app::{DB_OPERATION_TYPE, DbClientOperation, encode_operation};
use crate::doc::{CONNECTIONS_KEY, SAVED_QUERIES_KEY, SavedQuery};
use crate::model::{ConnectionProfile, Engine};
use crate::resource::DbState;

/// Wire type id under which the client's actions are encoded + registered.
pub const DB_ACTION_TYPE: &str = "db_client.action";

/// Every document-mutating operation the database client performs: the CRUD of
/// the saved-connection list. Serializable + deterministic, so the executor can
/// replay it on a peer and the undo log can reverse it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbAction {
    /// Add a saved connection profile. Carries every field **except the
    /// password**, which is session-only and never replicated.
    AddConnection {
        id: String,
        name: String,
        engine: String,
        host: String,
        port: u32,
        database: String,
        user: String,
        file_path: String,
    },
    /// Remove the saved connection profile with this id.
    RemoveConnection { id: String },
    /// Rename the saved connection profile with this id.
    RenameConnection { id: String, name: String },
    /// Save SQL as intentional, collaborative workspace content.
    SaveQuery {
        id: String,
        name: String,
        sql: String,
    },
    /// Hide a saved query while retaining its stable CRDT row as a tombstone.
    RemoveQuery { id: String },
    /// Replace the shared SQL buffer. Keystrokes stay on the text object;
    /// this is a snippet load or an accessibility set-value.
    ReplaceEditorSql { sql: String },
}

impl Action for DbAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            DbAction::AddConnection {
                id,
                name,
                engine,
                host,
                port,
                database,
                user,
                file_path,
            } => {
                let list = ensure_connections(doc)?;
                // Idempotent under replay: a profile id is added at most once.
                // A removed row remains as a tombstone so concurrent peers keep
                // one stable identity; re-adding that exact id revives the row
                // instead of creating a duplicate list entry.
                if let Some(index) = row_index_for_id(doc, &list, id) {
                    if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index) {
                        put_if_changed(doc, &row, "removed", false);
                    }
                    return Ok(());
                }
                let index = doc.length(&list);
                let row = doc.insert_object(&list, index, ObjType::Map)?;
                doc.put(&row, "id", id.as_str())?;
                doc.put(&row, "name", name.as_str())?;
                doc.put(&row, "engine", engine.as_str())?;
                doc.put(&row, "host", host.as_str())?;
                doc.put(&row, "port", i64::from(*port))?;
                doc.put(&row, "database", database.as_str())?;
                doc.put(&row, "user", user.as_str())?;
                doc.put(&row, "file_path", file_path.as_str())?;
                doc.put(&row, "removed", false)?;
            }
            DbAction::RemoveConnection { id } => {
                if let Some(list) = list_object(doc)
                    && let Some(index) = row_index_for_id(doc, &list, id)
                    && let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index)
                {
                    // Keep the row as a tombstone. Deleting a list element
                    // destroys the identity a concurrent rename/re-add needs
                    // to converge on and lets a later add duplicate it.
                    put_if_changed(doc, &row, "removed", true);
                }
            }
            DbAction::RenameConnection { id, name } => {
                if let Some(list) = list_object(doc)
                    && let Some(index) = row_index_for_id(doc, &list, id)
                    && let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index)
                {
                    put_if_changed(doc, &row, "name", name.as_str());
                }
            }
            DbAction::SaveQuery { id, name, sql } => {
                let list = ensure_saved_queries(doc)?;
                if let Some(index) = row_index_for_id(doc, &list, id)
                    && let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index)
                {
                    put_if_changed(doc, &row, "removed", false);
                    put_if_changed(doc, &row, "name", name.as_str());
                    put_if_changed(doc, &row, "sql", sql.as_str());
                    return Ok(());
                }
                let row = doc.insert_object(&list, doc.length(&list), ObjType::Map)?;
                doc.put(&row, "id", id.as_str())?;
                doc.put(&row, "name", name.as_str())?;
                doc.put(&row, "sql", sql.as_str())?;
                doc.put(&row, "removed", false)?;
            }
            DbAction::RemoveQuery { id } => {
                if let Some(list) = saved_query_list_object(doc)
                    && let Some(index) = row_index_for_id(doc, &list, id)
                    && let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index)
                {
                    put_if_changed(doc, &row, "removed", true);
                }
            }
            DbAction::ReplaceEditorSql { sql } => {
                crate::doc::EditorDoc::set_sql(doc, sql);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // A workspace's own connection list — applied locally and replicated to
        // any invited collaborator; no server ordering is needed.
        ExecutionLocus::ClientLocal
    }
}

/// The connection list object, if present as a List.
fn list_object(doc: &Doc) -> Option<ObjId> {
    match doc.get(&ROOT, CONNECTIONS_KEY) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

/// The connection list object, creating an empty one if absent.
fn ensure_connections(doc: &mut Doc) -> Result<ObjId, ActionError> {
    match list_object(doc) {
        Some(id) => Ok(id),
        None => Ok(doc.put_object(&ROOT, CONNECTIONS_KEY, ObjType::List)?),
    }
}

fn saved_query_list_object(doc: &Doc) -> Option<ObjId> {
    match doc.get(&ROOT, SAVED_QUERIES_KEY) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

fn ensure_saved_queries(doc: &mut Doc) -> Result<ObjId, ActionError> {
    match saved_query_list_object(doc) {
        Some(id) => Ok(id),
        None => Ok(doc.put_object(&ROOT, SAVED_QUERIES_KEY, ObjType::List)?),
    }
}

/// Index of the row whose `id` field equals `id`, if any.
fn row_index_for_id(doc: &Doc, list: &ObjId, id: &str) -> Option<usize> {
    for i in 0..doc.length(list) {
        if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(list, i)
            && let Ok(Some((Value::Scalar(v), _))) = doc.get(&row, "id")
            && let ScalarValue::Str(s) = v.as_ref()
            && s.as_str() == id
        {
            return Some(i);
        }
    }
    None
}

/// Register the client's action decoder for palette execution + remote replay.
/// Called once at app start from `main`.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DbAction>(DB_ACTION_TYPE);
}

/// Encode a [`DbAction`] into the replayable envelope both the on-screen
/// affordances and any future Ctrl+K entry would dispatch.
///
/// # Panics
/// Never in practice: every `DbAction` field is a plain `String` or `u32`,
/// so encoding only fails for a value that can't be serialized at all.
#[allow(
    clippy::expect_used,
    reason = "every DbAction field is a plain String or u32, so encoding cannot fail short of a value that isn't serializable at all — there is no meaningful EncodedAction to fall back to"
)]
pub fn encode(action: &DbAction) -> EncodedAction {
    EncodedAction::encode(DB_ACTION_TYPE, action).expect("DbAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth for the on-screen affordances
// in `crate::app`.
// ===========================================================================

/// Build the action that saves `profile`. The password is intentionally
/// dropped: it is session-only and must never enter the shared document.
pub fn add_connection_action(profile: &ConnectionProfile) -> DbAction {
    DbAction::AddConnection {
        id: profile.id.to_string(),
        name: profile.name.clone(),
        engine: profile.engine.as_key().to_string(),
        host: profile.host.clone(),
        port: u32::from(profile.port),
        database: profile.database.clone(),
        user: profile.user.clone(),
        file_path: profile.file_path.clone(),
    }
}

/// Build the action that removes the saved connection with `id`.
pub fn remove_connection_action(id: Uuid) -> DbAction {
    DbAction::RemoveConnection { id: id.to_string() }
}

/// Build the action that renames the saved connection with `id`.
pub fn rename_connection_action(id: Uuid, name: &str) -> DbAction {
    DbAction::RenameConnection {
        id: id.to_string(),
        name: name.to_string(),
    }
}

/// Build a stable-id saved-query action. The SQL remains collaborative text,
/// not a live server effect; running it stays a per-peer effect operation.
pub fn save_query_action(id: Uuid, name: &str, sql: &str) -> DbAction {
    DbAction::SaveQuery {
        id: id.to_string(),
        name: name.to_string(),
        sql: sql.to_string(),
    }
}

pub fn remove_query_action(id: Uuid) -> DbAction {
    DbAction::RemoveQuery { id: id.to_string() }
}

/// True when `engine` is one this build knows how to open (kept next to the
/// action so the modal and the decoder agree).
pub fn known_engine(engine: &str) -> bool {
    matches!(
        Engine::from_key(engine),
        Engine::Sqlite | Engine::Postgres | Engine::MySql
    )
}

/// Document actions bound from shared state for [`tilo_resource::publish`].
pub(crate) fn document_bound_actions(state: &DbState) -> Vec<ActionSpec> {
    let sql = state.editor_sql.clone();
    if sql.trim().is_empty() {
        return Vec::new();
    }
    vec![
        ActionSpec::prompted(
            "save_query",
            "Save current query",
            "Query name",
            "Saved query",
            ExecutionLocus::ClientLocal,
            move |name| encode(&save_query_action(Uuid::new_v4(), name, &sql)),
        )
        .reversible(),
    ]
}

/// Connection and saved-query children for [`tilo_resource::publish`].
pub(crate) fn document_children(state: &DbState) -> Vec<Resource> {
    let mut children: Vec<Resource> = state
        .connections
        .iter()
        .map(connection_document_resource)
        .collect();
    children.extend(
        state
            .saved_queries
            .iter()
            .map(saved_query_document_resource),
    );
    children
}

/// Root Effect: run prompted SQL against the active connection.
pub(crate) fn root_local_operations() -> Vec<ActionSpec> {
    let mut query = ActionSpec::prompted_operation(
        "query",
        "Run SQL",
        "SQL query",
        "SELECT …",
        OperationKind::Effect,
        DB_OPERATION_TYPE,
        |sql| {
            encode_operation(&DbClientOperation::RunQuery {
                // Resolved to the active connection at execution time.
                connection_id: String::new(),
                sql: sql.to_string(),
            })
        },
    );
    // Prompted SQL may be a read, but agents cannot soundly classify an
    // arbitrary dialect statement from this transport envelope. Declare the
    // conservative write/confirmation contract; the editor keeps its lighter
    // in-app path for clearly read-only SQL.
    query = query
        .execution(ExecutionMode::Background { cancellable: true })
        .returns(ResultSpec::Job)
        .destructive();
    vec![query]
}

fn connection_document_resource(connection: &ConnectionProfile) -> Resource {
    let id = format!("connection:{}", connection.id);
    let mut resource = Resource::new(&id, "database.connection", &connection.name);
    if let Ok(action) = ActionSpec::operation(
        "connect",
        "Connect",
        OperationKind::Effect,
        DB_OPERATION_TYPE,
        &DbClientOperation::Connect {
            connection_id: connection.id.to_string(),
        },
    ) {
        resource = resource.with_action(action.retry_policy(RetryPolicy::Idempotent));
    }
    if let Ok(action) = ActionSpec::immediate(
        "remove",
        "Remove saved connection",
        DB_ACTION_TYPE,
        &remove_connection_action(connection.id),
    ) {
        resource = resource.with_action(action.destructive());
    }
    let connection_id = connection.id;
    resource = resource.with_action(
        ActionSpec::prompted(
            "rename",
            "Rename",
            "Connection name",
            &connection.name,
            ExecutionLocus::ClientLocal,
            move |name| encode(&rename_connection_action(connection_id, name)),
        )
        .reversible()
        .retry_policy(RetryPolicy::Idempotent),
    );
    resource
}

fn saved_query_document_resource(query: &SavedQuery) -> Resource {
    let id = format!("saved-query:{}", query.id);
    let mut resource = Resource::new(&id, "database.saved-query", &query.name);
    if let Ok(action) = ActionSpec::operation(
        "run",
        "Run saved query",
        OperationKind::Effect,
        DB_OPERATION_TYPE,
        &DbClientOperation::RunQuery {
            connection_id: String::new(),
            sql: query.sql.clone(),
        },
    ) {
        resource = resource.with_action(action.destructive());
    }
    if let Ok(action) = ActionSpec::immediate(
        "remove",
        "Remove saved query",
        DB_ACTION_TYPE,
        &remove_query_action(query.id),
    ) {
        resource = resource.with_action(action.destructive());
    }
    resource
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &DbState) -> Vec<Resource> {
    vec![ui::tilo_resource::publish::<crate::resource::DbClient>(
        "database", "Database", state,
    )]
}

fn state_from_doc(doc: &AutoCommit) -> DbState {
    use crate::doc::{ConnectionsDoc, EditorDoc, SavedQueriesDoc};
    DbState {
        connections: ConnectionsDoc::read(doc),
        saved_queries: SavedQueriesDoc::read(doc),
        editor_sql: EditorDoc::read_sql(doc),
    }
}

fn find_published(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_published(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

// Inherent resource publication. The framework door is `tilo_resource::publish`;
// these keep Widget::resources / exact-id resolve available to tests and the
// standalone shot path, with live connection availability overlaid on Run SQL.
impl crate::app::DbClientApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut roots = provide_resources(&state_from_doc(doc));
        if let Some(root) = roots.first_mut()
            && let Some(query) = root.actions.iter_mut().find(|action| action.id == "query")
            && !self.session_connected()
        {
            query.contract.availability = tilo_actions::Availability::Unavailable {
                code: "not_connected".into(),
                reason: "Open a saved connection before running SQL".into(),
            };
        }
        roots
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        find_published(&self.publish_resources(doc), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ConnectionsDoc;
    use crate::model::Engine;
    use automerge::{ActorId, AutoCommit};
    use tilo_automerge::ApplyIncoming;

    fn profile() -> ConnectionProfile {
        let mut p = ConnectionProfile::new("Analytics", Engine::Postgres);
        p.host = "db.internal".into();
        p.port = 6543;
        p.database = "analytics".into();
        p.user = "reader".into();
        p.password = "s3cret".into();
        p
    }

    const SAVED_SQL: &str = "SELECT count(*) FROM orders";

    /// A workspace with two saved connections, one saved query and SQL in the
    /// editor, written through the same actions and text path the app uses.
    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let mut shop = ConnectionProfile::new("Shop", Engine::MySql);
        shop.host = "mysql.staging".into();
        for action in [
            add_connection_action(&profile()),
            add_connection_action(&shop),
            save_query_action(Uuid::new_v4(), "Daily totals", SAVED_SQL),
        ] {
            action.apply(&mut doc).unwrap();
        }
        crate::doc::EditorDoc::set_sql(&mut doc, "SELECT * FROM customers");
        doc.commit();
        doc
    }

    /// What [`tilo_resource::publish`] owns for `doc`: workspace door, connection
    /// and saved-query children, save-query / run-SQL palette ops.
    fn resources_for(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        provide_resources(&state_from_doc(doc))
    }

    fn first_connection(doc: &AutoCommit) -> Uuid {
        ConnectionsDoc::read(doc)[0].id
    }

    fn saved_query(doc: &AutoCommit) -> Uuid {
        crate::doc::SavedQueriesDoc::read(doc)[0].id
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: {
            let mut registry = ActionRegistry::new();
            register(&mut registry);
            registry
        },
        resources: |doc| resources_for(doc),
        palette: ["Remove saved connection", "Remove saved query", "Save current query", "Connect"],
        replay: [
            add_connection => |_| encode(&add_connection_action(&ConnectionProfile::new(
                "Local",
                Engine::Sqlite,
            ))),
            rename_connection => |doc| encode(&rename_connection_action(
                first_connection(doc),
                "Prod analytics",
            )),
            save_query => |_| encode(&save_query_action(Uuid::new_v4(), "Top", "SELECT 1")),
        ],
        undo: [
            remove_connection => |doc| encode(&remove_connection_action(first_connection(doc))),
            remove_query => |doc| encode(&remove_query_action(saved_query(doc))),
            rename_connection => |doc| encode(&rename_connection_action(
                first_connection(doc),
                "Prod analytics",
            )),
        ],
        encodes: [
            remove_connection => ("database.connection", "remove", |doc, _| {
                encode(&remove_connection_action(first_connection(doc)))
            }),
            rename_connection => ("database.connection", "rename", arg: "Prod analytics", |doc, _| {
                encode(&rename_connection_action(first_connection(doc), "Prod analytics"))
            }),
            remove_query => ("database.saved-query", "remove", |doc, _| {
                encode(&remove_query_action(saved_query(doc)))
            }),
        ],
    }

    /// Connection actions write, replay idempotently, rename and tombstone profiles, and passwords never enter the document.
    #[test]
    fn connection_actions_persist_profiles_without_secrets() {
        // --- phase: add_connection_writes_the_profile_to_the_doc ---
        // `AddConnection` through the real `Action::apply` path writes the profile
        // into `root["connections"]` and it reads back with every field intact.
        {
            let mut doc = AutoCommit::new();
            let p = profile();
            add_connection_action(&p).apply(&mut doc).unwrap();

            let conns = ConnectionsDoc::read(&doc);
            assert_eq!(conns.len(), 1);
            let c = &conns[0];
            assert_eq!(c.id, p.id);
            assert_eq!(c.name, "Analytics");
            assert_eq!(c.engine, Engine::Postgres);
            assert_eq!(c.host, "db.internal");
            assert_eq!(c.port, 6543);
            assert_eq!(c.database, "analytics");
            assert_eq!(c.user, "reader");
        }

        // --- phase: password_never_enters_the_document ---
        // The password is NEVER written to the shared document: it is `#[serde(skip)]`
        // and the add builder drops it, so the read-back profile carries an empty
        // password and no document key anywhere holds the secret.
        {
            let mut doc = AutoCommit::new();
            let p = profile();
            assert_eq!(
                p.password, "s3cret",
                "precondition: the profile has a secret"
            );
            add_connection_action(&p).apply(&mut doc).unwrap();

            // Read-back password is empty.
            let conns = ConnectionsDoc::read(&doc);
            assert!(
                conns[0].password.is_empty(),
                "read-back profile has no password"
            );

            // And the raw serialized document contains the secret nowhere.
            let bytes = doc.save();
            assert!(
                !bytes.windows(6).any(|w| w == b"s3cret"),
                "the saved document must not contain the plaintext password"
            );
        }

        // --- phase: add_is_idempotent_on_replay ---
        // Add is idempotent under replay: applying the same `AddConnection` twice
        // (as a peer merge or an executor replay might) leaves exactly one row.
        {
            let mut doc = AutoCommit::new();
            let action = add_connection_action(&profile());
            action.apply(&mut doc).unwrap();
            action.apply(&mut doc).unwrap();
            assert_eq!(ConnectionsDoc::read(&doc).len(), 1);
        }

        // --- phase: remove_connection_keeps_a_stable_tombstone ---
        // `RemoveConnection` hides exactly the addressed profile while retaining
        // its stable CRDT row as a tombstone for concurrent peers.
        {
            let mut doc = AutoCommit::new();
            let a = profile();
            let mut b = ConnectionProfile::new("Shop", Engine::MySql);
            b.host = "mysql.staging".into();
            add_connection_action(&a).apply(&mut doc).unwrap();
            add_connection_action(&b).apply(&mut doc).unwrap();
            assert_eq!(ConnectionsDoc::read(&doc).len(), 2);

            remove_connection_action(a.id).apply(&mut doc).unwrap();
            let left = ConnectionsDoc::read(&doc);
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].id, b.id, "the other profile survives removal");
            let list = ConnectionsDoc::list(&doc).expect("connection list exists");
            assert_eq!(doc.length(&list), 2, "removed row remains a tombstone");
            let (_, removed_row) = ConnectionsDoc::find(&doc, a.id).expect("tombstone keeps id");
            assert!(
                matches!(
                    doc.get(&removed_row, "removed"),
                    Ok(Some((Value::Scalar(value), _))) if value.as_ref() == &ScalarValue::Boolean(true)
                ),
                "tombstone records removal without deleting the row"
            );
        }

        // --- phase: rename_connection_updates_the_name ---
        // `RenameConnection` updates the addressed profile's name in place.
        {
            let mut doc = AutoCommit::new();
            let p = profile();
            add_connection_action(&p).apply(&mut doc).unwrap();
            rename_connection_action(p.id, "Prod analytics")
                .apply(&mut doc)
                .unwrap();
            assert_eq!(ConnectionsDoc::read(&doc)[0].name, "Prod analytics");
        }
    }

    /// Saved-query actions keep a tombstone that revives its id, and concurrent saves converge without losing rows.
    #[test]
    fn saved_query_actions_tombstone_and_converge() {
        // --- phase: saved_query_actions_keep_a_tombstone_and_revive_its_id ---
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            save_query_action(id, "Daily totals", "SELECT count(*) FROM orders")
                .apply(&mut doc)
                .unwrap();
            remove_query_action(id).apply(&mut doc).unwrap();
            assert!(crate::doc::SavedQueriesDoc::read(&doc).is_empty());

            save_query_action(id, "Daily totals", "SELECT count(*) FROM orders")
                .apply(&mut doc)
                .unwrap();
            let queries = crate::doc::SavedQueriesDoc::read(&doc);
            assert_eq!(queries.len(), 1);
            assert_eq!(queries[0].id, id);
            let list = crate::doc::SavedQueriesDoc::list(&doc).expect("query list exists");
            assert_eq!(doc.length(&list), 1, "revival must not duplicate a row");
        }

        // --- phase: concurrent_saved_queries_converge_without_losing_stable_rows ---
        {
            let mut base = AutoCommit::new().with_actor(ActorId::from([1; 16].as_slice()));
            // Establish the collaborative collection and a common Automerge
            // history before peers make independent inserts, just as an invited
            // collaborator would.
            save_query_action(Uuid::new_v4(), "Shared seed", "SELECT 1")
                .apply(&mut base)
                .unwrap();
            base.commit();
            let mut left = base.fork().with_actor(ActorId::from([2; 16].as_slice()));
            let mut right = base.fork().with_actor(ActorId::from([3; 16].as_slice()));
            let left_id = Uuid::new_v4();
            let right_id = Uuid::new_v4();

            save_query_action(left_id, "Daily totals", "SELECT count(*) FROM orders")
                .apply(&mut left)
                .unwrap();
            save_query_action(
                right_id,
                "Recent customers",
                "SELECT * FROM customers LIMIT 10",
            )
            .apply(&mut right)
            .unwrap();
            left.commit();
            right.commit();
            left.merge_incoming(&mut right).unwrap();
            let mut merged_left = left.fork();
            right.merge_incoming(&mut merged_left).unwrap();

            let visible = crate::doc::SavedQueriesDoc::read(&left);
            assert_eq!(visible.len(), 3);
            assert!(visible.iter().any(|query| query.id == left_id));
            assert!(visible.iter().any(|query| query.id == right_id));
            assert_eq!(left.get_heads(), right.get_heads(), "two peers converge");
        }
    }
}
