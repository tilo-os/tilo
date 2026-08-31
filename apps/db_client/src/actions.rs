//! The database client's action model.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
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
//! keystroke as an action would only duplicate that.
//!
//! ## Presentation is never an action
//!
//! Which connection is active, which table is selected, the current tab, the
//! grid sort/scroll and the responsive sidebar flap are per-peer view state on
//! the widget — presentation the recipe says must never be an action.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{Action, ActionError, ActionRegistry, Doc, EncodedAction, ExecutionLocus};
use ui::doc::put_if_changed;

use crate::doc::CONNECTIONS_KEY;
use crate::model::{ConnectionProfile, Engine};

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
                if row_index_for_id(doc, &list, id).is_some() {
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
            }
            DbAction::RemoveConnection { id } => {
                if let Some(list) = list_object(doc)
                    && let Some(index) = row_index_for_id(doc, &list, id)
                {
                    doc.delete(&list, index)?;
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

/// True when `engine` is one this build knows how to open (kept next to the
/// action so the modal and the decoder agree).
pub fn known_engine(engine: &str) -> bool {
    matches!(
        Engine::from_key(engine),
        Engine::Sqlite | Engine::Postgres | Engine::MySql
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ConnectionsDoc;
    use crate::model::Engine;
    use automerge::AutoCommit;

    fn profile() -> ConnectionProfile {
        let mut p = ConnectionProfile::new("Analytics", Engine::Postgres);
        p.host = "db.internal".into();
        p.port = 6543;
        p.database = "analytics".into();
        p.user = "reader".into();
        p.password = "s3cret".into();
        p
    }

    /// `AddConnection` through the real `Action::apply` path writes the profile
    /// into `root["connections"]` and it reads back with every field intact.
    #[test]
    fn add_connection_writes_the_profile_to_the_doc() {
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

    /// The password is NEVER written to the shared document: it is `#[serde(skip)]`
    /// and the add builder drops it, so the read-back profile carries an empty
    /// password and no document key anywhere holds the secret.
    #[test]
    fn password_never_enters_the_document() {
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

    /// Add is idempotent under replay: applying the same `AddConnection` twice
    /// (as a peer merge or an executor replay might) leaves exactly one row.
    #[test]
    fn add_is_idempotent_on_replay() {
        let mut doc = AutoCommit::new();
        let action = add_connection_action(&profile());
        action.apply(&mut doc).unwrap();
        action.apply(&mut doc).unwrap();
        assert_eq!(ConnectionsDoc::read(&doc).len(), 1);
    }

    /// `RemoveConnection` deletes exactly the addressed profile.
    #[test]
    fn remove_connection_deletes_the_row() {
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
        assert_eq!(left[0].id, b.id, "the other profile survives the delete");
    }

    /// `RenameConnection` updates the addressed profile's name in place.
    #[test]
    fn rename_connection_updates_the_name() {
        let mut doc = AutoCommit::new();
        let p = profile();
        add_connection_action(&p).apply(&mut doc).unwrap();
        rename_connection_action(p.id, "Prod analytics")
            .apply(&mut doc)
            .unwrap();
        assert_eq!(ConnectionsDoc::read(&doc)[0].name, "Prod analytics");
    }

    /// Actions replay byte-identically on an independent replica: a peer that
    /// merges the change converges to the same heads (the collaboration
    /// guarantee the action model exists to give).
    #[test]
    fn connection_actions_replicate_to_a_peer() {
        let mut doc = AutoCommit::new();
        add_connection_action(&profile()).apply(&mut doc).unwrap();
        doc.commit();
        let base = doc.save();

        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(
            ConnectionsDoc::read(&peer).len(),
            1,
            "the peer sees the saved connection after merge"
        );
        assert_eq!(doc.get_heads(), peer.get_heads(), "replicas converge");
    }
}
