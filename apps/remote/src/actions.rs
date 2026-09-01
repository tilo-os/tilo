//! The Remote app's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! ## The split
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) sorts every
//! interaction into exactly one of two buckets. A *document-mutating* operation
//! becomes an [`Action`]: a serializable value that deterministically changes
//! the shared Automerge document, so undo and byte-identical multi-peer replay
//! come for free. Everything else — presentation and one-off network I/O —
//! stays plain app code.
//!
//! - **Actions — the saved-connection store.** The durable, collaborative part
//!   of Remote is the list of saved machines (see [`crate::doc`]): a laptop that
//!   adds a host makes it appear on the desktop, renames merge, deletions
//!   propagate. So adding / editing, renaming, favouriting, and deleting a saved
//!   connection profile are document-mutating operations — the [`RemoteAction`]
//!   variants below. Each carries the freshly-minted id (never `Uuid::new_v4()`
//!   inside [`Action::apply`]) and the **absolute** target value, so `apply` is a
//!   pure function of `(payload, document)` and replays identically on every
//!   replica. A stray action against a missing connection is a **no-op**, never
//!   an error, so a bad replay can't wedge a peer.
//!
//! - **External operations.** Opening a live session is exported as an explicit
//!   effect on its connection resource. The framebuffer, input stream, and
//!   credential auth remain app-owned one-off I/O and touch no CRDT. And
//!   presentation — which screen is showing, the selected
//!   row, the in-progress add/edit draft, scale / fullscreen, the nav rail — is
//!   per-peer view state. None of it is an action.
//!
//! The resource tree mirrors that: every saved connection is a presented
//! resource, and rename / favourite / delete are the [`ActionSpec`]s Ctrl+K
//! offers for it. With no saved connections the app publishes nothing.

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, Resource, ResourceProvider, ResultSpec,
    RetryPolicy,
};

use crate::doc::RemoteDoc;
use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind, Presence};

/// Registry `type_id` for every Remote action.
pub const REMOTE_ACTION_TYPE: &str = "remote.action";
pub const REMOTE_CONNECT_OPERATION_TYPE: &str = "remote.operation.connect";

fn connect_spec(connection: &Connection) -> ActionSpec {
    let spec = ActionSpec::operation(
        "connect",
        "Connect",
        OperationKind::Effect,
        REMOTE_CONNECT_OPERATION_TYPE,
        &connection.id,
    )
    .expect("RemoteOperation is always serializable")
    .with_description("Open a live session to this saved connection")
    .with_contract(ActionContract {
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Resource {
            resource_kind: "remote.session".into(),
        },
        tags: vec!["network".into(), "session".into()],
        ..ActionContract::default()
    });
    if connection.presence == Presence::Offline {
        spec.unavailable("host_offline", "The saved host is currently offline")
    } else {
        spec
    }
}

/// Every collaborative operation Remote exports on the shared document. Each
/// mutates the saved-connection store in `root["connections"]`; each carries
/// everything the mutation needs so [`Action::apply`] reads no wall-clock and no
/// RNG and replays byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemoteAction {
    /// Add a new saved connection, or overwrite the editable fields of an
    /// existing one (the add/edit form's Save). `id` is minted at authoring
    /// time. The favourite flag, presence, and last-used descriptor are *not*
    /// carried here — they're preserved across an edit (or seeded for a new
    /// connection) by reading the current document, so editing a host never
    /// clobbers a collaborator's favourite.
    UpsertConnection {
        id: ConnectionId,
        name: String,
        host: String,
        port: u16,
        user: String,
        kind: ConnectionKind,
        auth: AuthMethod,
        group: String,
    },
    /// Rename a saved connection. Splices the collaborative `name` text so two
    /// concurrent renames merge rather than clobber.
    RenameConnection { id: ConnectionId, name: String },
    /// Set the favourite flag on a saved connection to an absolute target.
    SetFavorite { id: ConnectionId, favorite: bool },
    /// Delete a saved connection from the store.
    DeleteConnection { id: ConnectionId },
}

impl Action for RemoteAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            RemoteAction::UpsertConnection {
                id,
                name,
                host,
                port,
                user,
                kind,
                auth,
                group,
            } => {
                // Preserve the collaboratively-owned fields across an edit; seed
                // them for a brand-new connection. This is a deterministic read
                // of the current document (same on every replica), never a clock
                // or RNG — so replay stays byte-identical.
                let existing = RemoteDoc::list(doc).into_iter().find(|c| &c.id == id);
                let (favorite, presence, last_used) = match existing {
                    Some(e) => (e.favorite, e.presence, e.last_used),
                    None => (false, Presence::Unknown, "Never".to_string()),
                };
                let conn = Connection {
                    id: *id,
                    name: name.clone(),
                    host: host.clone(),
                    port: *port,
                    user: user.clone(),
                    kind: *kind,
                    auth: *auth,
                    last_used,
                    presence,
                    group: group.clone(),
                    favorite,
                };
                RemoteDoc::upsert(doc, &conn);
            }
            RemoteAction::RenameConnection { id, name } => {
                RemoteDoc::rename(doc, *id, name);
            }
            RemoteAction::SetFavorite { id, favorite } => {
                RemoteDoc::set_favorite(doc, *id, *favorite);
            }
            RemoteAction::DeleteConnection { id } => {
                RemoteDoc::remove(doc, *id);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // A personal / collaborative saved-connection edit: this client applies +
        // replicates, peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register Remote's action decoder for palette execution + remote replay.
/// Called once at app start.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<RemoteAction>(REMOTE_ACTION_TYPE);
}

/// Encode any [`RemoteAction`] into the wire form the palette and the on-screen
/// affordances both dispatch — the single point that turns a Remote action into
/// a replayable envelope.
///
/// # Panics
///
/// Panics if `action` fails to serialize. [`RemoteAction`] is a plain,
/// non-recursive enum of owned strings, ids, and booleans, so this cannot
/// happen for any value this module constructs.
#[allow(
    clippy::expect_used,
    reason = "RemoteAction always serializes (derived impl over plain data); \
              returning Result here would ripple through every call site in \
              app.rs and this module for an error path that can't occur"
)]
pub fn encode(action: &RemoteAction) -> EncodedAction {
    EncodedAction::encode(REMOTE_ACTION_TYPE, action).expect("RemoteAction is always serializable")
}

/// Build the "add/edit this connection" action from the fields the form owns —
/// the single source of truth shared by the form's Save button and any future
/// palette add flow.
pub fn upsert_action(c: &Connection) -> RemoteAction {
    RemoteAction::UpsertConnection {
        id: c.id,
        name: c.name.clone(),
        host: c.host.clone(),
        port: c.port,
        user: c.user.clone(),
        kind: c.kind,
        auth: c.auth,
        group: c.group.clone(),
    }
}

/// Build the "set favourite to `favorite`" action — shared by the on-screen star
/// and the Ctrl+K row so both dispatch the byte-identical envelope.
pub fn set_favorite_action(id: ConnectionId, favorite: bool) -> RemoteAction {
    RemoteAction::SetFavorite { id, favorite }
}

/// Build the "delete this connection" action — shared by the on-screen delete
/// affordance and the Ctrl+K row.
pub fn delete_action(id: ConnectionId) -> RemoteAction {
    RemoteAction::DeleteConnection { id }
}

/// Build the "rename this connection" action from a new name.
pub fn rename_action(id: ConnectionId, name: &str) -> RemoteAction {
    RemoteAction::RenameConnection {
        id,
        name: name.to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Build an immediate [`ActionSpec`] wrapping a [`RemoteAction`] — the resource
/// tree's per-connection buttons (favourite, delete) both go through this so the
/// encode-can't-fail invariant lives in one place rather than at each call site.
#[allow(
    clippy::expect_used,
    reason = "wraps ActionSpec::immediate, which cannot fail for RemoteAction \
              — see encode()'s doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &RemoteAction) -> ActionSpec {
    ActionSpec::immediate(id, label, REMOTE_ACTION_TYPE, action).expect("encode remote action")
}

/// Every saved connection, exported as a Ctrl+K resource with its rename /
/// favourite / delete actions. Level-triggered: recomputed from `doc` each
/// frame, so the labels and the favourite target always track the live state
/// (favourite flips to the opposite of the stored value, exactly as the
/// on-screen star does).
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    RemoteDoc::list(doc)
        .into_iter()
        .map(|c| {
            let id = c.id;
            let label = if c.name.trim().is_empty() {
                c.host.clone()
            } else {
                c.name.clone()
            };
            Resource::new(
                format!("remote.connection:{id}"),
                "remote.connection",
                label,
            )
            .with_action(connect_spec(&c))
            .with_action(
                ActionSpec::prompted(
                    "rename",
                    "Rename connection",
                    "New name",
                    "connection name",
                    ExecutionLocus::ClientLocal,
                    move |text| encode(&rename_action(id, text)),
                )
                .reversible(),
            )
            .with_action(
                immediate_spec(
                    "favorite",
                    if c.favorite {
                        "Remove from favourites"
                    } else {
                        "Add to favourites"
                    },
                    &set_favorite_action(id, !c.favorite),
                )
                .reversible(),
            )
            .with_action(
                immediate_spec("delete", "Delete connection", &delete_action(id))
                    .destructive()
                    .reversible(),
            )
        })
        .collect()
}

impl ResourceProvider for crate::app::RemoteApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::{ActionInput, LocalTransport};
    use uuid::Uuid;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Dispatch an action through the executor, exactly as the on-screen
    /// affordance path does (decode from the wire, apply through a local
    /// transport).
    fn dispatch(doc: &mut AutoCommit, action: &RemoteAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn sample(id: ConnectionId, name: &str) -> Connection {
        Connection {
            id,
            name: name.into(),
            host: "10.0.0.9".into(),
            port: 22,
            user: "admin".into(),
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            last_used: "2h ago".into(),
            presence: Presence::Online,
            group: "lab".into(),
            favorite: false,
        }
    }

    #[test]
    fn upsert_action_writes_a_connection() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        dispatch(&mut doc, &upsert_action(&sample(id, "Build Box")));
        let back = RemoteDoc::list(&doc);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, id);
        assert_eq!(back[0].name, "Build Box");
        assert_eq!(back[0].host, "10.0.0.9");
        assert_eq!(
            back[0].last_used, "Never",
            "a fresh connection seeds last-used"
        );
    }

    #[test]
    fn upsert_edit_preserves_favourite_and_presence() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        let mut c = sample(id, "Box");
        c.favorite = true;
        c.presence = Presence::Online;
        c.last_used = "5m ago".into();
        RemoteDoc::upsert(&mut doc, &c);

        // The form edits only the editable fields; favourite/presence/last_used
        // are collaboratively owned and must survive the edit.
        let mut edited = c.clone();
        edited.name = "Box (renamed)".into();
        edited.host = "10.0.0.10".into();
        dispatch(&mut doc, &upsert_action(&edited));

        let back = &RemoteDoc::list(&doc)[0];
        assert_eq!(back.name, "Box (renamed)");
        assert_eq!(back.host, "10.0.0.10");
        assert!(back.favorite, "edit preserved the favourite flag");
        assert_eq!(back.presence, Presence::Online, "edit preserved presence");
        assert_eq!(back.last_used, "5m ago", "edit preserved last-used");
    }

    #[test]
    fn rename_action_changes_only_the_name() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        let c = sample(id, "Before");
        RemoteDoc::upsert(&mut doc, &c);
        dispatch(&mut doc, &rename_action(id, "After"));
        let back = &RemoteDoc::list(&doc)[0];
        assert_eq!(back.name, "After");
        assert_eq!(back.host, c.host, "rename leaves the address intact");
    }

    #[test]
    fn favourite_action_sets_the_absolute_value() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut doc, &sample(id, "Box"));
        dispatch(&mut doc, &set_favorite_action(id, true));
        assert!(RemoteDoc::list(&doc)[0].favorite);
        dispatch(&mut doc, &set_favorite_action(id, false));
        assert!(!RemoteDoc::list(&doc)[0].favorite);
    }

    #[test]
    fn delete_action_removes_the_connection() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut doc, &sample(id, "Box"));
        dispatch(&mut doc, &delete_action(id));
        assert!(RemoteDoc::list(&doc).is_empty());
    }

    /// A stray action against a connection that isn't in the document is a
    /// no-op, not an error — a bad replay can't wedge a peer, touch a real
    /// record, or conjure a phantom machine. Each action is dispatched once
    /// against a store that holds a real connection but targets a *different*,
    /// absent id — the production shape (one action per user event).
    #[test]
    fn actions_against_a_missing_connection_are_noops() {
        let ghost = Uuid::new_v4();
        for action in [
            rename_action(ghost, "Ghost"),
            set_favorite_action(ghost, false),
            delete_action(ghost),
        ] {
            let mut doc = AutoCommit::new();
            let real = Uuid::new_v4();
            let mut c = sample(real, "Real Box");
            c.favorite = true;
            RemoteDoc::upsert(&mut doc, &c);

            dispatch(&mut doc, &action);

            let back = RemoteDoc::list(&doc);
            assert_eq!(back.len(), 1, "{action:?} must not add a connection");
            assert_eq!(back[0].id, real);
            assert_eq!(
                back[0].name, "Real Box",
                "{action:?} left the real record intact"
            );
            assert!(
                back[0].favorite,
                "{action:?} left the real favourite flag intact"
            );
        }
    }

    /// Ctrl+K publishes one resource per saved connection, each exporting the
    /// connect / rename / favourite / delete operations, and the one palette lists them.
    /// Drives the real resource-builder and the real [`ui::ActionPalette`].
    #[test]
    fn ctrl_k_publishes_a_resource_per_connection() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut doc, &sample(id, "Build Box"));

        let resources = provide_resources(&doc);
        assert_eq!(resources.len(), 1, "one resource per saved connection");
        let r = &resources[0];
        assert_eq!(r.kind, "remote.connection");
        assert_eq!(r.label, "Build Box");
        assert_eq!(r.id.as_str(), &format!("remote.connection:{id}"));
        let ids: Vec<&str> = r.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["connect", "rename", "favorite", "delete"]);
        let connect = r
            .actions
            .iter()
            .find(|action| action.id == "connect")
            .unwrap();
        assert_eq!(connect.kind, OperationKind::Effect);
        assert!(matches!(
            connect.contract.execution,
            ExecutionMode::Background { cancellable: true }
        ));
        assert!(
            r.actions
                .iter()
                .find(|a| a.id == "delete")
                .unwrap()
                .contract
                .confirmation
                != qos_actions::ConfirmationPolicy::None
        );
        let encoded = immediate_encode(connect);
        assert_eq!(encoded.type_id, REMOTE_CONNECT_OPERATION_TYPE);
        assert_eq!(encoded.payload, format!("\"{id}\"").into_bytes());

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Rename")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Connect")), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("favourites")),
            "{labels:?}"
        );
        assert!(labels.iter().any(|l| l.contains("Delete")), "{labels:?}");
    }

    /// The palette's favourite action tracks the live document: with a favourited
    /// connection seeded, the exported action targets the opposite (unfavourite).
    #[test]
    fn palette_favourite_targets_the_opposite_of_current() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        let mut c = sample(id, "Box");
        c.favorite = true;
        RemoteDoc::upsert(&mut doc, &c);

        let resources = provide_resources(&doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "favorite")
            .unwrap();
        assert_eq!(spec.label, "Remove from favourites");
        assert_eq!(
            immediate_encode(spec),
            encode(&set_favorite_action(id, false)),
            "favourited → the row flips it back off",
        );
    }

    /// The full undo covenant on the real executor path: dispatched through the
    /// executor the action mutates the document and records exactly one undo
    /// boundary, and Ctrl+Z (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut state.doc, &sample(id, "Box"));
        state.doc.commit();
        assert!(!RemoteDoc::list(&state.doc)[0].favorite);
        let before_heads = state.doc.get_heads();

        let action = set_favorite_action(id, true);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            RemoteDoc::list(&state.doc)[0].favorite,
            "the action set the flag"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            !RemoteDoc::list(&state.doc)[0].favorite,
            "undo reverted the favourite change and nothing else",
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same value.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = AutoCommit::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut a, &sample(id, "Box"));
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = rename_action(id, "Renamed");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(RemoteDoc::list(&b)[0].name, "Renamed");
    }

    /// Single source of truth: the palette's favourite + delete specs encode
    /// byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        RemoteDoc::upsert(&mut doc, &sample(id, "Box")); // not favourited
        let resources = provide_resources(&doc);
        let actions = &resources[0].actions;

        let fav = actions.iter().find(|a| a.id == "favorite").unwrap();
        assert_eq!(
            immediate_encode(fav),
            encode(&set_favorite_action(id, true)),
            "favourite is one source of truth",
        );
        let del = actions.iter().find(|a| a.id == "delete").unwrap();
        assert_eq!(
            immediate_encode(del),
            encode(&delete_action(id)),
            "delete is one source of truth",
        );
    }
}
