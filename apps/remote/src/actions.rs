//! The Remote app's exported **actions**, and the operations it publishes
//! beside them.
//!
//! ## The split
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) sorts every
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
//!   an error, so a bad replay can't wedge a peer. Opening a session records
//!   when the machine was last used ([`RemoteAction::MarkUsed`]), which follows
//!   the person to their other devices but is nobody's undo step.
//!
//! - **External operations.** Opening a live session is exported as an explicit
//!   effect on its connection resource. The framebuffer, input stream, and
//!   credential auth remain app-owned one-off I/O and touch no CRDT; a
//!   connection's password or key passphrase lives in this device's secret
//!   store. And presentation — which screen is showing, the selected row, the
//!   in-progress add/edit draft, scale / fullscreen, the nav rail — is per-peer
//!   view state, as is whether a machine answered. None of it is an action.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, ChangeKind, Doc, EncodedAction, ExecutionLocus,
};
use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

use crate::doc::CONNECTIONS;
use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind};

/// Registry `type_id` for every Remote action.
pub const REMOTE_ACTION_TYPE: &str = "remote.action";

/// What Remote does that is no edit of the saved connections: showing them,
/// opening and closing live sessions, typing into one, and the SSH host-key
/// decisions this device keeps for itself.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "remote.effect")]
pub enum RemoteEffect {
    /// Show connections.
    #[effect(view, idempotent)]
    Open,
    /// New connection.
    #[effect(view)]
    New,
    /// Open connection settings.
    #[effect(view, row)]
    EditConnection {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Connect.
    #[effect(row, job, privileged, permission = "read")]
    Connect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Disconnect session.
    #[effect(row, idempotent)]
    Disconnect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Send text to session.
    #[effect(job, privileged)]
    SendText {
        #[param("The saved connection whose session receives it")]
        connection: ConnectionId,
        #[param("Text to deliver to the connected SSH terminal")]
        text: String,
    },
    /// Trust SSH host key.
    #[effect(row, job, privileged)]
    TrustHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
        #[param("The host key observed on first contact")]
        observed: crate::ssh::HostKeyIdentity,
    },
    /// Forget SSH host key.
    #[effect(row, job, destructive)]
    ForgetHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
}

/// Open a connection's settings.
pub(crate) fn open_spec(id: ConnectionId) -> ActionSpec {
    RemoteEffect::EditConnection { connection: id }.bound()
}

/// Open a live session to a connection, when it can start one.
pub(crate) fn connect_spec(connection: &Connection) -> ActionSpec {
    let readiness = crate::session::connection_readiness(connection);
    let mut spec = RemoteEffect::Connect {
        connection: connection.id,
    }
    .bound()
    .with_description(format!(
        "Open a live session to this saved connection. {}",
        readiness.detail(connection.kind)
    ));
    spec.contract.tags = vec!["network".into(), "session".into()];
    if readiness.can_connect() {
        spec
    } else {
        spec.unavailable(
            readiness.code(),
            if readiness == crate::session::TransportReadiness::InvalidProfile {
                connection
                    .profile_issue()
                    .unwrap_or_else(|| readiness.detail(connection.kind))
            } else {
                readiness.detail(connection.kind)
            },
        )
    }
}

/// A host-key decision stays on this device.
const HOST_KEY_TAGS: &[&str] = &["ssh", "host-key", "device-private"];

/// `spec`, tagged with what it touches.
fn tagged(mut spec: ActionSpec, tags: &[&str]) -> ActionSpec {
    spec.contract.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
    spec
}

/// The open session: disconnect it, and the SSH host-key decision its
/// first contact or a changed key asks for. Text is sent with the
/// `send-text` effect, naming the connection.
pub fn session_row(
    connection: &Connection,
    host_key_issue: Option<&crate::ssh::HostKeyIssue>,
) -> LiveRow {
    let id = connection.id;
    let row = LiveRow::new(
        format!("remote.session:{id}"),
        "remote.session",
        format!("{} session", connection.name),
    )
    .operation(tagged(
        RemoteEffect::Disconnect { connection: id }.bound(),
        &["network", "session"],
    ));
    match host_key_issue {
        Some(crate::ssh::HostKeyIssue::FirstContact { observed }) => row.operation(tagged(
            RemoteEffect::TrustHostKey {
                connection: id,
                observed: observed.clone(),
            }
            .bound()
            .with_description(format!(
                "Explicitly trust {} {} for this device only after out-of-band verification.",
                observed.algorithm, observed.fingerprint
            )),
            HOST_KEY_TAGS,
        )),
        Some(crate::ssh::HostKeyIssue::Changed { .. }) => row.operation(tagged(
            RemoteEffect::ForgetHostKey { connection: id }
                .bound()
                .with_description(
                    "Remove this device's stale SSH host-key pin; reconnect to inspect a replacement key.",
                ),
            HOST_KEY_TAGS,
        )),
        _ => row,
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
    /// time. The favourite flag and when it was last used are *not* carried
    /// here — an edit leaves them as they are, so editing a host never
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
    /// Record that a session to the connection was opened at `at_ms`
    /// (milliseconds since the Unix epoch). The later of two concurrent
    /// records wins on every replica.
    MarkUsed { id: ConnectionId, at_ms: i64 },
}

/// The connection map, created on first write.
fn connections_map(doc: &mut Doc) -> Result<ObjId, ActionError> {
    match doc.get(&ROOT, CONNECTIONS)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(&ROOT, CONNECTIONS, ObjType::Map)?),
    }
}

/// The map of an existing connection.
fn connection(doc: &mut Doc, id: ConnectionId) -> Result<Option<ObjId>, ActionError> {
    let conns = connections_map(doc)?;
    Ok(match doc.get(&conns, id.to_string().as_str())? {
        Some((Value::Object(ObjType::Map), conn)) => Some(conn),
        _ => None,
    })
}

/// Splice `name` into the connection's collaborative name text.
fn put_name(doc: &mut Doc, conn: &ObjId, name: &str) -> Result<(), ActionError> {
    let text = match doc.get(conn, "name")? {
        Some((Value::Object(ObjType::Text), text)) => text,
        _ => doc.put_object(conn, "name", ObjType::Text)?,
    };
    ui::doc::splice_text_diffed(doc, &text, name);
    Ok(())
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
                // An edit leaves the favourite and last-used fields alone; a
                // new connection starts unfavourited.
                let conn = if let Some(conn) = connection(doc, *id)? {
                    conn
                } else {
                    let conns = connections_map(doc)?;
                    let conn = doc.put_object(&conns, id.to_string().as_str(), ObjType::Map)?;
                    doc.put(&conn, "favorite", false)?;
                    conn
                };
                put_name(doc, &conn, name)?;
                ui::doc::put_if_changed(doc, &conn, "host", host.as_str());
                ui::doc::put_if_changed(doc, &conn, "port", i64::from(*port));
                ui::doc::put_if_changed(doc, &conn, "user", user.as_str());
                ui::doc::put_if_changed(doc, &conn, "kind", kind.tag());
                ui::doc::put_if_changed(doc, &conn, "auth", auth.label());
                ui::doc::put_if_changed(doc, &conn, "group", group.as_str());
            }
            RemoteAction::RenameConnection { id, name } => {
                if let Some(conn) = connection(doc, *id)? {
                    put_name(doc, &conn, name)?;
                }
            }
            RemoteAction::SetFavorite { id, favorite } => {
                if let Some(conn) = connection(doc, *id)? {
                    ui::doc::put_if_changed(doc, &conn, "favorite", *favorite);
                }
            }
            RemoteAction::DeleteConnection { id } => {
                let conns = connections_map(doc)?;
                if connection(doc, *id)?.is_some() {
                    doc.delete(&conns, id.to_string().as_str())?;
                }
            }
            RemoteAction::MarkUsed { id, at_ms } => {
                if let Some(conn) = connection(doc, *id)? {
                    let last = ui::doc::read_i64_coerced(doc, &conn, "last_used_ms");
                    if last.is_none_or(|last| last < *at_ms) {
                        doc.put(&conn, "last_used_ms", *at_ms)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn change_kind(&self) -> ChangeKind {
        match self {
            // When a machine was last used is a marker, not an edit anyone
            // takes back.
            RemoteAction::MarkUsed { .. } => ChangeKind::Housekeeping,
            _ => ChangeKind::Content,
        }
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

/// Build the "a session to this connection was opened now" action.
pub fn mark_used_action(id: ConnectionId, at_ms: i64) -> RemoteAction {
    RemoteAction::MarkUsed { id, at_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::RemoteDoc;
    use crate::resource::{Remote, connection_resource_id};
    use automerge::AutoCommit;
    use tilo_actions::{
        ConfirmationPolicy, OperationKind, ParamType, PermissionRequirement, ResultSpec,
        RetryPolicy, RiskLevel,
    };
    use tilo_actions::{LocalTransport, Resource};
    use ui::tilo_resource::Resource as _;
    use uuid::Uuid;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    /// Dispatch an action through the executor, exactly as the on-screen
    /// affordance path does (decode from the wire, apply through a local
    /// transport).
    fn dispatch(doc: &mut AutoCommit, action: &RemoteAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// What the framework publishes for `doc`'s saved connections.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<Remote>(
            Remote::KIND,
            Remote::LABEL,
            &Remote::read(doc).unwrap(),
        )]
    }

    const BUILD_BOX: ConnectionId = Uuid::from_u128(0xb01d);
    const FAVOURITE_BOX: ConnectionId = Uuid::from_u128(0xfa5);

    /// `connection`, saved through its actions.
    fn save(doc: &mut AutoCommit, connection: &Connection) {
        dispatch(doc, &upsert_action(connection));
        if connection.favorite {
            dispatch(doc, &set_favorite_action(connection.id, true));
        }
    }

    /// Two saved connections, one favourited.
    fn seeded_connections() -> AutoCommit {
        let mut doc = AutoCommit::new();
        save(&mut doc, &sample(BUILD_BOX, "Build Box"));
        let mut favourite = sample(FAVOURITE_BOX, "Media Server");
        favourite.favorite = true;
        favourite.kind = ConnectionKind::Vnc;
        favourite.auth = AuthMethod::Password;
        favourite.port = 5900;
        save(&mut doc, &favourite);
        doc.commit();
        doc
    }

    ui::action_contract_tests! {
        seed: seeded_connections(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "Rename connection",
            "Open connection settings",
            "Pin connection",
            "Delete connection",
        ],
        replay: [
            rename => |_| encode(&rename_action(BUILD_BOX, "Renamed")),
            upsert_edit => |_| {
                let mut edited = sample(FAVOURITE_BOX, "Media Server (den)");
                edited.host = "10.0.0.42".into();
                encode(&upsert_action(&edited))
            },
            mark_used => |_| encode(&mark_used_action(BUILD_BOX, 1_700_000_000_000)),
        ],
        undo: [
            favourite => |_| encode(&set_favorite_action(BUILD_BOX, true)),
            add_connection => |_| encode(&upsert_action(&sample(Uuid::from_u128(0xadd), "New Box"))),
        ],
        encodes: [
            delete => (&connection_resource_id(BUILD_BOX), "delete-connection", |_, _| {
                encode(&delete_action(BUILD_BOX))
            }),
            rename => (&connection_resource_id(BUILD_BOX), "rename-connection", arg: "Lab Box", |_, _| {
                encode(&rename_action(BUILD_BOX, "Lab Box"))
            }),
        ],
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
            last_used_ms: None,
            group: "lab".into(),
            favorite: false,
        }
    }

    #[test]
    fn upsert_and_rename_actions_write_only_their_fields() {
        eprintln!("phase upsert_action_round_trips_every_field");
        {
            for kind in ConnectionKind::all() {
                let mut doc = AutoCommit::new();
                let mut c = sample(Uuid::new_v4(), "Build Box");
                c.kind = kind;
                dispatch(&mut doc, &upsert_action(&c));
                assert_eq!(RemoteDoc::list(&doc), vec![c], "{kind:?} round-trips");
            }
        }

        eprintln!("phase upsert_edit_preserves_favourite_and_last_used");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            let c = sample(id, "Box");
            save(&mut doc, &c);
            dispatch(&mut doc, &set_favorite_action(id, true));
            dispatch(&mut doc, &mark_used_action(id, 5_000));

            // The form edits only the editable fields; favourite and last
            // used must survive the edit.
            let mut edited = c.clone();
            edited.name = "Box (renamed)".into();
            edited.host = "10.0.0.10".into();
            dispatch(&mut doc, &upsert_action(&edited));

            let back = &RemoteDoc::list(&doc)[0];
            assert_eq!(back.name, "Box (renamed)");
            assert_eq!(back.host, "10.0.0.10");
            assert!(back.favorite, "edit preserved the favourite flag");
            assert_eq!(back.last_used_ms, Some(5_000), "edit preserved last used");
        }

        eprintln!("phase rename_action_changes_only_the_name");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            let c = sample(id, "Before");
            save(&mut doc, &c);
            dispatch(&mut doc, &rename_action(id, "After"));
            let back = &RemoteDoc::list(&doc)[0];
            assert_eq!(back.name, "After");
            assert_eq!(back.host, c.host, "rename leaves the address intact");
        }

        eprintln!("phase appending_one_char_to_the_name_is_bounded");
        {
            // The name is a collaborative Text; renaming by one character
            // splices only that character.
            let mut doc = AutoCommit::new();
            let mut c = sample(Uuid::new_v4(), "x");
            c.name = "n".repeat(300);
            save(&mut doc, &c);
            let before = doc.save().len();
            c.name.push('!');
            dispatch(&mut doc, &upsert_action(&c));
            let grew = doc.save().len() - before;
            assert_eq!(RemoteDoc::list(&doc)[0].name, c.name);
            assert!(
                grew < 200,
                "one more character grew the doc by {grew} bytes"
            );
        }

        eprintln!("phase concurrent_saves_merge");
        {
            use tilo_automerge::ApplyIncoming;
            let mut a = AutoCommit::new();
            save(&mut a, &sample(Uuid::new_v4(), "A"));
            let mut b = AutoCommit::load(&a.save()).unwrap();
            save(&mut b, &sample(Uuid::new_v4(), "B"));
            let mut b = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b).unwrap();
            let mut names: Vec<_> = RemoteDoc::list(&a).into_iter().map(|c| c.name).collect();
            names.sort();
            assert_eq!(names, ["A", "B"]);
        }
    }

    #[test]
    fn favourite_delete_and_last_used_are_absolute_and_ignore_missing_connections() {
        eprintln!("phase favourite_action_sets_the_absolute_value");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            save(&mut doc, &sample(id, "Box"));
            dispatch(&mut doc, &set_favorite_action(id, true));
            assert!(RemoteDoc::list(&doc)[0].favorite);
            dispatch(&mut doc, &set_favorite_action(id, false));
            assert!(!RemoteDoc::list(&doc)[0].favorite);
        }

        eprintln!("phase delete_action_removes_the_connection");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            save(&mut doc, &sample(id, "Box"));
            dispatch(&mut doc, &delete_action(id));
            assert!(RemoteDoc::list(&doc).is_empty());
        }

        eprintln!("phase the_latest_use_wins_and_is_no_undo_step");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            save(&mut doc, &sample(id, "Box"));
            dispatch(&mut doc, &mark_used_action(id, 2_000));
            dispatch(&mut doc, &mark_used_action(id, 1_000));
            assert_eq!(
                RemoteDoc::list(&doc)[0].last_used_ms,
                Some(2_000),
                "a late, older record does not move last used back"
            );
            assert_eq!(
                mark_used_action(id, 1).change_kind(),
                ChangeKind::Housekeeping
            );
        }

        // A stray action against a connection that isn't in the document is a
        // no-op, not an error — a bad replay can't wedge a peer, touch a real
        // record, or conjure a phantom machine.
        eprintln!("phase actions_against_a_missing_connection_are_noops");
        {
            let ghost = Uuid::new_v4();
            for action in [
                rename_action(ghost, "Ghost"),
                set_favorite_action(ghost, false),
                delete_action(ghost),
                mark_used_action(ghost, 1),
            ] {
                let mut doc = AutoCommit::new();
                let real = Uuid::new_v4();
                let mut c = sample(real, "Real Box");
                c.favorite = true;
                save(&mut doc, &c);

                dispatch(&mut doc, &action);

                let back = RemoteDoc::list(&doc);
                assert_eq!(back, vec![c], "{action:?} left the real record intact");
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn published_connections_offer_only_connectable_profiles() {
        eprintln!("phase every_connection_is_an_item_with_its_actions");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            save(&mut doc, &sample(id, "Build Box"));

            let resources = published(&doc);
            let root = &resources[0];
            let actions: Vec<_> = root
                .actions
                .iter()
                .map(|action| (action.id.as_str(), action.kind))
                .collect();
            assert!(
                actions.contains(&("add-connection", OperationKind::Document)),
                "{actions:?}"
            );
            assert_eq!(root.children.len(), 1, "one saved connection child");
            let r = &root.children[0];
            assert_eq!(r.label, "Build Box");
            assert_eq!(r.id.as_str(), connection_resource_id(id));
            let ids: Vec<&str> = r.actions.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    "edit-connection",
                    "connect",
                    "rename-connection",
                    "set-favorite",
                    "delete-connection"
                ]
            );
            let connect = &r.actions[1];
            assert_eq!(connect.kind, OperationKind::Effect);
            assert_eq!(
                connect.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(connect.contract.result, tilo_actions::ResultSpec::Job);
            assert_ne!(
                r.actions[4].contract.confirmation,
                tilo_actions::ConfirmationPolicy::None,
                "deleting asks first"
            );
            let encoded = ui::testing::action_contract::spec_encoding(connect, None);
            assert_eq!(encoded.type_id, RemoteEffect::OPERATION);
            assert_eq!(
                RemoteEffect::from_payload(&encoded.payload).unwrap(),
                RemoteEffect::Connect { connection: id }
            );

            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.open();
            let labels = palette.result_labels();
            assert!(
                !labels.iter().any(|l| l.ends_with(" — Connect")),
                "a gated transport must not appear invokable: {labels:?}"
            );
        }

        eprintln!("phase add_connection_validates_and_mints_an_id");
        {
            let add = Remote::declarations()
                .into_iter()
                .find(|decl| decl.id == "add-connection")
                .unwrap();
            let args = |json: serde_json::Value| {
                tilo_actions::ActionArgs::bind_json(&add.params, json.as_object().unwrap()).unwrap()
            };
            let built = (add.build)(
                &args(serde_json::json!({"name": "Lab", "host": "lab.local", "kind": "ssh"})),
                0,
            )
            .unwrap();
            assert!(matches!(
                built,
                RemoteAction::UpsertConnection { port: 22, kind: ConnectionKind::Ssh, ref name, .. }
                    if name == "Lab"
            ));
            assert!(
                (add.build)(
                    &args(serde_json::json!({"name": "Lab", "host": "friendly", "kind": "tilo"})),
                    0,
                )
                .is_err(),
                "a Tilo connection needs an exact endpoint id"
            );
        }

        eprintln!("phase tilo_connect_action_is_available_without_the_ssh_vnc_dev_gate");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = iroh_base::SecretKey::generate().public().to_string();
            assert!(
                connect_spec(&connection)
                    .contract
                    .availability
                    .is_available()
            );
        }

        eprintln!("phase malformed_tilo_profile_is_not_offered_as_connectable");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = "friendly-name".into();
            let spec = connect_spec(&connection);
            assert!(matches!(
                &spec.contract.availability,
                tilo_actions::Availability::Unavailable { code, reason }
                    if code == "invalid_connection_profile"
                        && reason.contains("exact certified endpoint")
            ));
        }

        eprintln!("phase legacy_rdp_resource_names_the_missing_backend_instead_of_a_generic_gate");
        {
            let mut connection = sample(Uuid::new_v4(), "Old Windows host");
            connection.kind = ConnectionKind::Rdp;
            let mut doc = AutoCommit::new();
            save(&mut doc, &connection);
            let resources = published(&doc);
            let connect = resources[0].children[0]
                .actions
                .iter()
                .find(|action| action.id == "connect")
                .expect("RDP profile stays manageable");
            assert!(matches!(
                &connect.contract.availability,
                tilo_actions::Availability::Unavailable { code, reason }
                    if code == "transport_backend_unavailable"
                        && reason.contains("but not RDP")
            ));
            assert!(
                connect
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("but not RDP")),
                "search and agents receive the actionable reason"
            );
        }
    }

    fn session_actions(
        connection: &Connection,
        issue: Option<&crate::ssh::HostKeyIssue>,
    ) -> (String, Vec<ActionSpec>) {
        let row = session_row(connection, issue).into_resource();
        (row.id.as_str().to_owned(), row.actions)
    }

    #[test]
    fn session_effects_declare_permissions_lifecycle_and_trust() {
        eprintln!("phase remote_effects_declare_confirmation_permissions_and_non_repeatability");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = iroh_base::SecretKey::generate().public().to_string();
            let connect = connect_spec(&connection);
            assert_eq!(connect.contract.permission, PermissionRequirement::Read);
            assert_eq!(connect.contract.risk, RiskLevel::Privileged);
            assert_eq!(connect.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(connect.contract.retry, RetryPolicy::AtMostOnce);
            assert_eq!(connect.contract.result, ResultSpec::Job);

            let input = RemoteEffect::specs()
                .into_iter()
                .find(|action| action.id == "send-text")
                .expect("typed input action");
            assert_eq!(input.contract.permission, PermissionRequirement::Write);
            assert_eq!(input.contract.risk, RiskLevel::Privileged);
            assert_eq!(input.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(input.contract.retry, RetryPolicy::AtMostOnce);
            assert_eq!(input.contract.result, ResultSpec::Job);
        }

        eprintln!("phase active_session_exports_lifecycle_and_typed_input");
        {
            let id = Uuid::new_v4();
            let (row, actions) = session_actions(&sample(id, "Build Box"), None);
            assert_eq!(row, format!("remote.session:{id}"));
            let disconnect = actions.iter().find(|a| a.id == "disconnect").unwrap();
            assert_eq!(disconnect.contract.retry, RetryPolicy::Idempotent);
            let input = RemoteEffect::specs()
                .into_iter()
                .find(|a| a.id == "send-text")
                .unwrap();
            assert!(
                input
                    .params()
                    .iter()
                    .any(|param| param.name == "text" && param.ty == ParamType::Text)
            );
        }

        eprintln!("phase first_contact_exports_an_explicit_device_private_trust_action");
        {
            let issue = crate::ssh::HostKeyIssue::FirstContact {
                observed: crate::ssh::HostKeyIdentity {
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: "SHA256:example".into(),
                },
            };
            let trust = session_actions(&sample(Uuid::new_v4(), "Build Box"), Some(&issue))
                .1
                .into_iter()
                .find(|action| action.id == "trust-host-key")
                .expect("first contact publishes a trust action");
            assert_eq!(trust.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(trust.contract.risk, RiskLevel::Privileged);
            assert!(
                trust
                    .contract
                    .tags
                    .iter()
                    .any(|tag| tag == "device-private")
            );
            assert!(
                trust
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("SHA256:example"))
            );
        }
    }
}
