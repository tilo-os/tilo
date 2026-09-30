//! The Remote app's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
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
//!   an error, so a bad replay can't wedge a peer.
//!
//! - **External operations.** Opening a live session is exported as an explicit
//!   effect on its connection resource. The framebuffer, input stream, and
//!   credential auth remain app-owned one-off I/O and touch no CRDT. And
//!   presentation — which screen is showing, the selected
//!   row, the in-progress add/edit draft, scale / fullscreen, the nav rail — is
//!   per-peer view state. None of it is an action.
//!
//! The resource tree mirrors that: a stable connection-list root exposes the
//! open/new views and owns one child per saved connection. Each child exposes
//! open/edit, connect, rename, favourite, and delete. This hierarchy remains
//! discoverable even for an empty first-run list, so search and agents can open
//! the creation UI instead of needing a mouse-only bootstrap.

use automerge::AutoCommit;

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    ConfirmationPolicy, Doc, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind,
    ParamType, PermissionRequirement, Resource, ResultSpec, RetryPolicy, RiskLevel,
};

use crate::doc::RemoteDoc;
use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind, Presence};

/// Registry `type_id` for every Remote action.
pub const REMOTE_ACTION_TYPE: &str = "remote.action";
pub const REMOTE_OPERATION_TYPE: &str = "remote.operation";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemoteOperation {
    ShowConnections,
    NewConnectionForm,
    EditConnection {
        connection_id: ConnectionId,
    },
    Connect {
        connection_id: ConnectionId,
    },
    Disconnect {
        connection_id: ConnectionId,
    },
    SendText {
        connection_id: ConnectionId,
        text: String,
    },
    TrustSshHostKey {
        connection_id: ConnectionId,
        observed: crate::ssh::HostKeyIdentity,
    },
    ForgetSshHostKey {
        connection_id: ConnectionId,
    },
}

pub fn operation_kind(operation: &RemoteOperation) -> OperationKind {
    match operation {
        RemoteOperation::ShowConnections
        | RemoteOperation::NewConnectionForm
        | RemoteOperation::EditConnection { .. } => OperationKind::View,
        RemoteOperation::Connect { .. }
        | RemoteOperation::Disconnect { .. }
        | RemoteOperation::SendText { .. }
        | RemoteOperation::TrustSshHostKey { .. }
        | RemoteOperation::ForgetSshHostKey { .. } => OperationKind::Effect,
    }
}

fn encode_operation(operation: &RemoteOperation) -> EncodedAction {
    EncodedAction::encode_operation(REMOTE_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: REMOTE_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

/// Decode an operation published by this app.
///
/// # Errors
///
/// Returns the JSON decoding error when `type_id` identifies Remote but the
/// payload is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<RemoteOperation>, String> {
    if type_id != REMOTE_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn operation_spec(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &RemoteOperation,
) -> ActionSpec {
    let fallback = operation.clone();
    ActionSpec::operation(id, label, kind, REMOTE_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        ActionSpec::typed_operation(
            id,
            label,
            "",
            kind,
            REMOTE_OPERATION_TYPE,
            Vec::new(),
            move |_| encode_operation(&fallback),
        )
    })
}

fn connect_spec(connection: &Connection) -> ActionSpec {
    let spec = operation_spec(
        "connect",
        "Connect",
        OperationKind::Effect,
        &RemoteOperation::Connect {
            connection_id: connection.id,
        },
    )
    .with_description(format!(
        "Open a live session to this saved connection. {}",
        crate::session::connection_readiness(connection).detail(connection.kind)
    ))
    .with_contract(ActionContract {
        permission: PermissionRequirement::Read,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Job,
        tags: vec!["network".into(), "session".into()],
        ..ActionContract::default()
    });
    let readiness = crate::session::connection_readiness(connection);
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

#[allow(
    clippy::too_many_lines,
    reason = "the session resource intentionally keeps its lifecycle and trust affordances together"
)]
pub fn session_resource(
    connection: &Connection,
    can_send_text: bool,
    host_key_issue: Option<&crate::ssh::HostKeyIssue>,
) -> Resource {
    let id = connection.id;
    let disconnect = ActionSpec::operation(
        "disconnect",
        "Disconnect session",
        OperationKind::Effect,
        REMOTE_OPERATION_TYPE,
        &RemoteOperation::Disconnect { connection_id: id },
    )
    .unwrap_or_else(|_| {
        ActionSpec::typed_operation(
            "disconnect",
            "Disconnect session",
            "",
            OperationKind::Effect,
            REMOTE_OPERATION_TYPE,
            Vec::new(),
            move |_| encode_operation(&RemoteOperation::Disconnect { connection_id: id }),
        )
    })
    .with_contract(ActionContract {
        retry: RetryPolicy::Idempotent,
        tags: vec!["network".into(), "session".into()],
        ..ActionContract::default()
    });
    let input = ActionSpec::typed_operation(
        "send-text",
        "Send text to session",
        "command or text",
        OperationKind::Effect,
        REMOTE_OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "text",
            ParamType::Text,
            "Text to deliver to the connected SSH terminal",
        )],
        move |args: &ActionArgs| {
            encode_operation(&RemoteOperation::SendText {
                connection_id: id,
                text: args.text("text").unwrap_or("").to_string(),
            })
        },
    )
    .with_contract(ActionContract {
        permission: PermissionRequirement::Write,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Job,
        tags: vec!["remote-input".into(), "session".into()],
        ..ActionContract::default()
    });
    let mut resource = Resource::new(
        format!("remote.session:{id}"),
        "remote.session",
        format!("{} session", connection.name),
    )
    .with_action(disconnect)
    .with_action(if can_send_text {
        input
    } else {
        input.unavailable(
            "terminal_not_connected",
            "Text input requires a connected SSH terminal",
        )
    });
    match host_key_issue {
        Some(crate::ssh::HostKeyIssue::FirstContact { observed }) => {
            let operation = RemoteOperation::TrustSshHostKey {
                connection_id: id,
                observed: observed.clone(),
            };
            resource = resource.with_action(
                operation_spec(
                    "trust-host-key",
                    "Trust SSH host key",
                    OperationKind::Effect,
                    &operation,
                )
                .with_description(format!(
                    "Explicitly trust {} {} for this device only after out-of-band verification.",
                    observed.algorithm, observed.fingerprint
                ))
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    risk: RiskLevel::Privileged,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::AtMostOnce,
                    execution: ExecutionMode::Background { cancellable: false },
                    result: ResultSpec::Job,
                    tags: vec!["ssh".into(), "host-key".into(), "device-private".into()],
                    ..ActionContract::default()
                }),
            );
        }
        Some(crate::ssh::HostKeyIssue::Changed { .. }) => {
            let operation = RemoteOperation::ForgetSshHostKey { connection_id: id };
            resource = resource.with_action(
                operation_spec("forget-host-key", "Forget SSH host key", OperationKind::Effect, &operation)
                    .with_description("Remove this device's stale SSH host-key pin; reconnect to inspect a replacement key.")
                    .with_contract(ActionContract {
                        permission: PermissionRequirement::Write,
                        risk: RiskLevel::Destructive,
                        confirmation: ConfirmationPolicy::Confirm,
                        retry: RetryPolicy::AtMostOnce,
                        execution: ExecutionMode::Background { cancellable: false },
                        result: ResultSpec::Job,
                        tags: vec!["ssh".into(), "host-key".into(), "device-private".into()],
                        ..ActionContract::default()
                    }),
            );
        }
        _ => {}
    }
    resource
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

/// The connection-list root and every saved child, exported to Ctrl+K with
/// view, network-effect, and document actions. Level-triggered: recomputed from `doc` each
/// frame, so the labels and the favourite target always track the live state
/// (favourite flips to the opposite of the stored value, exactly as the
/// on-screen star does).
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let connections = RemoteDoc::list(doc)
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
            .with_action(operation_spec(
                "open",
                "Open connection settings",
                OperationKind::View,
                &RemoteOperation::EditConnection { connection_id: id },
            ))
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
        .collect::<Vec<_>>();
    let workspace = Resource::new(
        "remote.connections",
        "remote.connection-list",
        "Remote connections",
    )
    .with_action(operation_spec(
        "open",
        "Show connections",
        OperationKind::View,
        &RemoteOperation::ShowConnections,
    ))
    .with_action(operation_spec(
        "new",
        "New connection",
        OperationKind::View,
        &RemoteOperation::NewConnectionForm,
    ))
    .with_children(connections);
    vec![workspace]
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::RemoteApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let state =
            <crate::resource::Remote as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Remote>(
            "remote.connections",
            "Remote connections",
            &state,
        );
        if let Some(session) = self.live_session_resources().pop() {
            root.children.push(session);
        }
        vec![root]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::LocalTransport;
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
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    const BUILD_BOX: ConnectionId = Uuid::from_u128(0xb01d);
    const FAVOURITE_BOX: ConnectionId = Uuid::from_u128(0xfa5);

    fn connection_address(id: ConnectionId) -> String {
        format!("remote.connection:{id}")
    }

    /// Two saved connections, one favourited, written through the document
    /// store the app persists connections with.
    fn seeded_connections() -> AutoCommit {
        let mut doc = AutoCommit::new();
        RemoteDoc::upsert(&mut doc, &sample(BUILD_BOX, "Build Box"));
        let mut favourite = sample(FAVOURITE_BOX, "Media Server");
        favourite.favorite = true;
        favourite.kind = ConnectionKind::Vnc;
        favourite.port = 5900;
        RemoteDoc::upsert(&mut doc, &favourite);
        doc.commit();
        doc
    }

    ui::action_contract_tests! {
        seed: seeded_connections(),
        registry: registry(),
        resources: |doc| crate::app::RemoteApp::new().publish_resources(doc),
        palette: [
            "Rename connection",
            "Open connection settings",
            "Add to favourites",
            "Remove from favourites",
            "Delete connection",
        ],
        replay: [
            rename => |_| encode(&rename_action(BUILD_BOX, "Renamed")),
            upsert_edit => |_| {
                let mut edited = sample(FAVOURITE_BOX, "Media Server (den)");
                edited.host = "10.0.0.42".into();
                encode(&upsert_action(&edited))
            },
        ],
        undo: [
            favourite => |_| encode(&set_favorite_action(BUILD_BOX, true)),
            add_connection => |_| encode(&upsert_action(&sample(Uuid::from_u128(0xadd), "New Box"))),
        ],
        encodes: [
            favourite => (&connection_address(BUILD_BOX), "favorite", |_, _| {
                encode(&set_favorite_action(BUILD_BOX, true))
            }),
            unfavourite => (&connection_address(FAVOURITE_BOX), "favorite", |_, _| {
                encode(&set_favorite_action(FAVOURITE_BOX, false))
            }),
            delete => (&connection_address(BUILD_BOX), "delete", |_, _| {
                encode(&delete_action(BUILD_BOX))
            }),
            rename => (&connection_address(BUILD_BOX), "rename", arg: "Lab Box", |_, _| {
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
            last_used: "2h ago".into(),
            presence: Presence::Online,
            group: "lab".into(),
            favorite: false,
        }
    }

    #[test]
    fn upsert_and_rename_actions_write_only_their_fields() {
        eprintln!("phase upsert_action_writes_a_connection");
        {
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

        eprintln!("phase upsert_edit_preserves_favourite_and_presence");
        {
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

        eprintln!("phase rename_action_changes_only_the_name");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            let c = sample(id, "Before");
            RemoteDoc::upsert(&mut doc, &c);
            dispatch(&mut doc, &rename_action(id, "After"));
            let back = &RemoteDoc::list(&doc)[0];
            assert_eq!(back.name, "After");
            assert_eq!(back.host, c.host, "rename leaves the address intact");
        }
    }

    #[test]
    fn favourite_and_delete_actions_are_absolute_and_ignore_missing_connections() {
        eprintln!("phase favourite_action_sets_the_absolute_value");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            RemoteDoc::upsert(&mut doc, &sample(id, "Box"));
            dispatch(&mut doc, &set_favorite_action(id, true));
            assert!(RemoteDoc::list(&doc)[0].favorite);
            dispatch(&mut doc, &set_favorite_action(id, false));
            assert!(!RemoteDoc::list(&doc)[0].favorite);
        }

        eprintln!("phase delete_action_removes_the_connection");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            RemoteDoc::upsert(&mut doc, &sample(id, "Box"));
            dispatch(&mut doc, &delete_action(id));
            assert!(RemoteDoc::list(&doc).is_empty());
        }

        // A stray action against a connection that isn't in the document is a
        // no-op, not an error — a bad replay can't wedge a peer, touch a real
        // record, or conjure a phantom machine. Each action is dispatched once
        // against a store that holds a real connection but targets a *different*,
        // absent id — the production shape (one action per user event).
        eprintln!("phase actions_against_a_missing_connection_are_noops");
        {
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

        // The palette's favourite action tracks the live document: with a favourited
        // connection seeded, the exported action targets the opposite (unfavourite).
        eprintln!("phase palette_favourite_targets_the_opposite_of_current");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            let mut c = sample(id, "Box");
            c.favorite = true;
            RemoteDoc::upsert(&mut doc, &c);

            let resources = provide_resources(&doc);
            let spec = resources[0].children[0]
                .actions
                .iter()
                .find(|a| a.id == "favorite")
                .unwrap();
            assert_eq!(spec.label, "Remove from favourites");
            assert_eq!(
                ui::testing::action_contract::spec_encoding(spec, None),
                encode(&set_favorite_action(id, false)),
                "favourited → the row flips it back off",
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn palette_resources_offer_only_connectable_profiles() {
        // Ctrl+K publishes one resource per saved connection, each exporting the
        // connect / rename / favourite / delete operations, and the one palette lists them.
        // Drives the real resource-builder and the real [`ui::ActionPalette`].
        eprintln!("phase ctrl_k_publishes_a_resource_per_connection");
        {
            let mut doc = AutoCommit::new();
            let id = Uuid::new_v4();
            RemoteDoc::upsert(&mut doc, &sample(id, "Build Box"));

            let resources = provide_resources(&doc);
            assert_eq!(resources.len(), 1, "one connection-list root");
            assert_eq!(resources[0].id.as_str(), "remote.connections");
            assert_eq!(
                resources[0]
                    .actions
                    .iter()
                    .map(|action| (action.id.as_str(), action.kind))
                    .collect::<Vec<_>>(),
                vec![("open", OperationKind::View), ("new", OperationKind::View)],
            );
            assert_eq!(resources[0].children.len(), 1, "one saved connection child");
            let r = &resources[0].children[0];
            assert_eq!(r.kind, "remote.connection");
            assert_eq!(r.label, "Build Box");
            assert_eq!(r.id.as_str(), &format!("remote.connection:{id}"));
            let ids: Vec<&str> = r.actions.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(ids, vec!["open", "connect", "rename", "favorite", "delete"]);
            let connect = r
                .actions
                .iter()
                .find(|action| action.id == "connect")
                .unwrap();
            assert_eq!(connect.kind, OperationKind::Effect);
            assert_eq!(
                connect.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(connect.contract.result, tilo_actions::ResultSpec::Job);
            assert_ne!(
                r.actions
                    .iter()
                    .find(|a| a.id == "delete")
                    .unwrap()
                    .contract
                    .confirmation,
                tilo_actions::ConfirmationPolicy::None
            );
            let encoded = ui::testing::action_contract::spec_encoding(connect, None);
            assert_eq!(encoded.type_id, REMOTE_OPERATION_TYPE);
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(RemoteOperation::Connect { connection_id: id })
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

        eprintln!("phase tilo_connect_action_is_available_without_the_ssh_vnc_dev_gate");
        {
            let id = Uuid::new_v4();
            let mut connection = sample(id, "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = iroh_base::SecretKey::generate().public().to_string();
            connection.presence = Presence::Online;
            assert!(
                connect_spec(&connection)
                    .contract
                    .availability
                    .is_available()
            );
        }

        eprintln!("phase malformed_q_os_profile_is_not_offered_as_a_connectable_resource");
        {
            let id = Uuid::new_v4();
            let mut connection = sample(id, "Tilo device");
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
            let id = Uuid::new_v4();
            let mut connection = sample(id, "Old Windows host");
            connection.kind = ConnectionKind::Rdp;
            let resources = {
                let mut doc = AutoCommit::new();
                RemoteDoc::upsert(&mut doc, &connection);
                provide_resources(&doc)
            };
            let resource = &resources[0].children[0];
            let connect = resource
                .actions
                .iter()
                .find(|action| action.id == "connect")
                .expect("RDP profile stays manageable");
            assert!(matches!(
                &connect.contract.availability,
                tilo_actions::Availability::Unavailable { code, reason }
                    if code == "transport_backend_unavailable"
                        && reason.contains("no RDP client backend")
            ));
            assert!(
                connect
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("no RDP client backend")),
                "search and agents receive the actionable reason"
            );
        }
    }

    #[test]
    fn session_effects_declare_permissions_lifecycle_and_trust() {
        eprintln!("phase remote_effects_declare_confirmation_permissions_and_non_repeatability");
        {
            let id = Uuid::new_v4();
            let mut connection = sample(id, "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = iroh_base::SecretKey::generate().public().to_string();
            let connect = connect_spec(&connection);
            assert_eq!(connect.contract.permission, PermissionRequirement::Read);
            assert_eq!(connect.contract.risk, RiskLevel::Privileged);
            assert_eq!(connect.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(connect.contract.retry, RetryPolicy::AtMostOnce);
            assert_eq!(connect.contract.result, ResultSpec::Job);

            let input = session_resource(&connection, true, None)
                .actions
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
            let session = session_resource(&sample(id, "Build Box"), false, None);
            assert_eq!(session.id.as_str(), format!("remote.session:{id}"));
            let disconnect = session
                .actions
                .iter()
                .find(|a| a.id == "disconnect")
                .unwrap();
            assert_eq!(disconnect.contract.retry, RetryPolicy::Idempotent);
            let input = session
                .actions
                .iter()
                .find(|a| a.id == "send-text")
                .unwrap();
            assert_eq!(input.params()[0].ty, ParamType::Text);
            assert!(!input.contract.availability.is_available());
        }

        eprintln!("phase first_contact_exports_an_explicit_device_private_trust_action");
        {
            let id = Uuid::new_v4();
            let issue = crate::ssh::HostKeyIssue::FirstContact {
                observed: crate::ssh::HostKeyIdentity {
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: "SHA256:example".into(),
                },
            };
            let trust = session_resource(&sample(id, "Build Box"), false, Some(&issue))
                .actions
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
