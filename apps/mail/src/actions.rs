//! Mail's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Mail performs on its shared mailbox state is
//! a [`MailAction`]: a small serializable value that deterministically mutates the
//! CRDT (see [`crate::doc`], where per-message flags, the local-folder registry,
//! and the draft live). Because that state already rides in the shared document,
//! an action replicates and replays for free, and feeds straight back into the
//! panes on the next `sync`. The exact same [`EncodedAction`] is what the on-screen
//! affordances (the row star, the reader's action bar, the context menu, Save
//! draft) and the Ctrl+K command palette both build and dispatch — one source of
//! truth, one replay path.
//!
//! ## The split
//!
//! **Document-mutating → an action:** mark read/unread, star, move to a folder
//! (archive / delete-to-trash / junk / move-to-inbox are all a move), add/remove a
//! label, create/rename/delete a local folder, and save/discard the draft.
//!
//! **External, non-deterministic → never an action:** IMAP fetch/sync, SMTP send,
//! and account auth all write mailbox state as *external ingestion* driven by the
//! network (see [`crate::engine`]); they can't replay byte-identically, so they
//! stay outside the action model. Sending a mail is an external effect; **saving**
//! a draft is an action.
//!
//! **Presentation / view state → never an action:** the compose WYSIWYG buffer
//! until it is saved or sent, the selection, the search query, which folder/pane is
//! open, and the read-pane scroll. Nothing to replicate.
//!
//! ## Determinism contract
//!
//! Messages are addressed by their id string and folders by their key; a payload
//! carries the exact ids and the exact target value, so [`Action::apply`] is a pure
//! function of `(payload, base document)` — no RNG, no wall-clock, no app state.
//! Freshly-minted ids (a new local folder) are minted at *authoring* time and baked
//! into the payload. Bulk operations over several messages are batched into one
//! action (one undo boundary, one actor-pinned change, one byte-identical replay),
//! and their ids are applied in the payload's given order. A malformed payload (an
//! unknown folder key) is a **no-op**, never an error, so a bad replay can't wedge
//! a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::MailDoc;
use crate::model::{Draft, Folder};

/// Registry `type_id` for every Mail action.
pub const MAIL_ACTION_TYPE: &str = "mail.action";
pub const MAIL_OPERATION_TYPE: &str = "mail.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailOperation {
    RefreshCurrentFolder,
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<MailOperation>, String> {
    if type_id != MAIL_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The persisted scalar fields of the in-progress draft, in exactly the shape
/// [`MailDoc::store_draft`] writes. Carrying the document shape keeps
/// [`Action::apply`] a pure write with no re-derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DraftData {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub html_body: Option<String>,
}

impl DraftData {
    pub fn from_draft(d: &Draft) -> Self {
        Self {
            to: d.to.clone(),
            cc: d.cc.clone(),
            bcc: d.bcc.clone(),
            subject: d.subject.clone(),
            body: d.body.clone(),
            html_body: d.html_body.clone(),
        }
    }

    fn to_draft(&self) -> Draft {
        Draft {
            to: self.to.clone(),
            cc: self.cc.clone(),
            bcc: self.bcc.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            html_body: self.html_body.clone(),
            attachments: Vec::new(),
            in_reply_to: None,
        }
    }
}

/// Every collaborative operation Mail exports on its shared mailbox state.
///
/// Determinism contract: ids and target values are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock and applies its ids
/// in the payload's given order. A malformed payload is a **no-op**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailAction {
    /// Set the read/unread flag on a batch of messages.
    SetRead { ids: Vec<String>, read: bool },
    /// Set the starred flag on a batch of messages.
    SetStarred { ids: Vec<String>, starred: bool },
    /// Move a batch of messages to `folder` (a folder key from
    /// [`Folder::as_key`]). Archive, delete-to-trash, junk, and move-to-inbox are
    /// all this with a specific target. An unknown key is a no-op.
    Move { ids: Vec<String>, folder: String },
    /// Add a label/tag to a batch of messages. Idempotent per message.
    AddLabel { ids: Vec<String>, label: String },
    /// Remove a label/tag from a batch of messages.
    RemoveLabel { ids: Vec<String>, label: String },
    /// Create a user-defined local folder with a stable id + display name (the id
    /// is minted at authoring time).
    CreateFolder { id: String, name: String },
    /// Rename a user-defined local folder. A no-op for an unknown id.
    RenameFolder { id: String, name: String },
    /// Delete a user-defined local folder from the registry.
    DeleteFolder { id: String },
    /// Save (create or overwrite) the in-progress draft.
    SaveDraft(DraftData),
    /// Discard the in-progress draft, clearing it from the document.
    DiscardDraft,
}

impl Action for MailAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            MailAction::SetRead { ids, read } => {
                for id in ids {
                    MailDoc::set_read(doc, id, *read);
                }
            }
            MailAction::SetStarred { ids, starred } => {
                for id in ids {
                    MailDoc::set_starred(doc, id, *starred);
                }
            }
            MailAction::Move { ids, folder } => {
                // An unknown folder key (e.g. a stale local-folder id) is a no-op
                // rather than a wrong move — the system `Folder` enum is total.
                if let Some(f) = Folder::from_key(folder) {
                    for id in ids {
                        MailDoc::set_folder_for(doc, id, f);
                    }
                }
            }
            MailAction::AddLabel { ids, label } => {
                for id in ids {
                    MailDoc::add_label(doc, id, label);
                }
            }
            MailAction::RemoveLabel { ids, label } => {
                for id in ids {
                    MailDoc::remove_label(doc, id, label);
                }
            }
            MailAction::CreateFolder { id, name } => {
                MailDoc::create_folder(doc, id, name);
            }
            MailAction::RenameFolder { id, name } => {
                MailDoc::rename_folder(doc, id, name);
            }
            MailAction::DeleteFolder { id } => {
                MailDoc::delete_folder(doc, id);
            }
            MailAction::SaveDraft(data) => {
                MailDoc::store_draft(doc, &data.to_draft());
            }
            MailAction::DiscardDraft => {
                MailDoc::clear_draft(doc);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Mail's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<MailAction>(MAIL_ACTION_TYPE);
}

/// Encode any [`MailAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Mail action into a
/// replayable envelope.
///
/// # Panics
/// Never in practice: [`MailAction`] is a plain derived-`Serialize` enum with
/// no non-serializable fields, so encoding it cannot fail.
#[allow(
    clippy::expect_used,
    reason = "MailAction is a plain derived-Serialize enum, so encoding cannot fail; an un-encodable action has no replayable form to fall back to"
)]
pub fn encode(action: &MailAction) -> EncodedAction {
    EncodedAction::encode(MAIL_ACTION_TYPE, action).expect("MailAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::state` / `crate::app`) and the Ctrl+K resource tree.
// ===========================================================================

/// Set the read flag on a batch of messages.
pub fn set_read_action(ids: Vec<String>, read: bool) -> MailAction {
    MailAction::SetRead { ids, read }
}

/// Set the starred flag on a batch of messages.
pub fn set_starred_action(ids: Vec<String>, starred: bool) -> MailAction {
    MailAction::SetStarred { ids, starred }
}

/// Move a batch of messages to a system folder.
pub fn move_action(ids: Vec<String>, folder: Folder) -> MailAction {
    MailAction::Move {
        ids,
        folder: folder.as_key().to_string(),
    }
}

/// Add a label to a batch of messages.
pub fn add_label_action(ids: Vec<String>, label: &str) -> MailAction {
    MailAction::AddLabel {
        ids,
        label: label.to_string(),
    }
}

/// Remove a label from a batch of messages.
pub fn remove_label_action(ids: Vec<String>, label: &str) -> MailAction {
    MailAction::RemoveLabel {
        ids,
        label: label.to_string(),
    }
}

/// Create a local folder with a caller-minted id.
pub fn create_folder_action(id: &str, name: &str) -> MailAction {
    MailAction::CreateFolder {
        id: id.to_string(),
        name: name.to_string(),
    }
}

/// Rename a local folder.
pub fn rename_folder_action(id: &str, name: &str) -> MailAction {
    MailAction::RenameFolder {
        id: id.to_string(),
        name: name.to_string(),
    }
}

/// Delete a local folder.
pub fn delete_folder_action(id: &str) -> MailAction {
    MailAction::DeleteFolder { id: id.to_string() }
}

/// Save (create or overwrite) the in-progress draft.
pub fn save_draft_action(draft: &Draft) -> MailAction {
    MailAction::SaveDraft(DraftData::from_draft(draft))
}

/// Discard the in-progress draft.
pub fn discard_draft_action() -> MailAction {
    MailAction::DiscardDraft
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One message surfaced as a resource, with the scalar state its actions need
/// resolved at authoring time (so "Mark read" builds the byte-identical action
/// the on-screen control does).
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub subject: String,
    pub read: bool,
    pub starred: bool,
    pub labels: Vec<String>,
}

/// The presentation context Mail hands [`provide_resources`]: the account label,
/// the currently-open folder and its messages, the user's local-folder registry,
/// and whether a draft is in progress. Everything here is derived from state the
/// document already holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct MailResourceCtx {
    pub mailbox_label: String,
    pub current_folder: Folder,
    /// Messages in the currently-open folder (the "open" scope), bounded.
    pub messages: Vec<MessageRow>,
    /// User-defined local folders `(id, name)`, sorted by id.
    pub custom_folders: Vec<(String, String)>,
    /// Whether a persisted draft exists (so "Discard draft" is offered).
    pub has_draft: bool,
    pub can_sync: bool,
}

/// # Panics
/// Never in practice: [`MailAction`] is a plain derived-`Serialize` enum with
/// no non-serializable fields, so encoding it cannot fail.
#[allow(
    clippy::expect_used,
    reason = "MailAction is a plain derived-Serialize enum, so encoding cannot fail; a spec that cannot encode its action would be an unusable palette entry"
)]
fn immediate_spec(id: &str, label: &str, action: &MailAction) -> ActionSpec {
    ActionSpec::immediate(id, label, MAIL_ACTION_TYPE, action)
        .expect("encode mail action")
        .reversible()
}

fn message_label(m: &MessageRow) -> String {
    let s = m.subject.trim();
    let base = if s.is_empty() { "(no subject)" } else { s };
    if m.starred {
        format!("★ {base}")
    } else {
        base.to_string()
    }
}

/// Build the resource tree Mail currently has open: the mailbox root (exporting
/// "New folder"), the open folder with its messages nested (each exporting
/// read / flag / move / delete + label actions), and one `mail.folder:<id>` per
/// user-defined local folder (rename / delete). The Drafts folder additionally
/// exports "Discard draft" when a draft is in progress. Every operation is the
/// same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &MailResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("mail.mailbox", "mail.mailbox", ctx.mailbox_label.clone());

    if let Ok(refresh) = ActionSpec::operation(
        "refresh",
        "Refresh current folder",
        OperationKind::Effect,
        MAIL_OPERATION_TYPE,
        &MailOperation::RefreshCurrentFolder,
    ) {
        let refresh = refresh.with_contract(ActionContract {
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            tags: vec!["network".into(), "mail".into(), "refresh".into()],
            ..ActionContract::default()
        });
        root = root.with_action(if ctx.can_sync {
            refresh
        } else {
            refresh.unavailable("account_offline", "No connected mail account is available")
        });
    }

    // Mailbox-level: create a new local folder. The id is minted once when the
    // name is committed, then baked into the envelope, so replay stays
    // deterministic.
    root = root.with_action(
        ActionSpec::prompted(
            "new-folder",
            "New folder",
            "Name",
            "folder name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let id = uuid::Uuid::new_v4().to_string();
                encode(&create_folder_action(&id, name.trim()))
            },
        )
        .reversible(),
    );

    // The currently-open folder, with its messages nested.
    let mut folder_node = Resource::new(
        format!("mail.folder:{}", ctx.current_folder.as_key()),
        "mail.folder",
        ctx.current_folder.title().to_string(),
    );
    if ctx.current_folder == Folder::Drafts && ctx.has_draft {
        folder_node = folder_node.with_action(
            immediate_spec("discard-draft", "Discard draft", &discard_draft_action()).destructive(),
        );
    }
    for m in &ctx.messages {
        let mut msg = Resource::new(
            format!("mail.message:{}", m.id),
            "mail.message",
            message_label(m),
        );

        let read_label = if m.read { "Mark unread" } else { "Mark read" };
        msg = msg.with_action(immediate_spec(
            "read",
            read_label,
            &set_read_action(vec![m.id.clone()], !m.read),
        ));

        let star_label = if m.starred { "Unstar" } else { "Star" };
        msg = msg.with_action(immediate_spec(
            "star",
            star_label,
            &set_starred_action(vec![m.id.clone()], !m.starred),
        ));

        msg = msg.with_action(immediate_spec(
            "archive",
            "Archive",
            &move_action(vec![m.id.clone()], Folder::Archive),
        ));
        msg = msg.with_action(
            immediate_spec(
                "delete",
                "Delete",
                &move_action(vec![m.id.clone()], Folder::Trash),
            )
            .destructive(),
        );

        // Add a label (prompted). The id is captured presentation-side.
        let mid = m.id.clone();
        msg = msg.with_action(ActionSpec::prompted(
            "add-label",
            "Add label…",
            "Label",
            "label…",
            ExecutionLocus::ClientLocal,
            move |label: &str| encode(&add_label_action(vec![mid.clone()], label.trim())),
        ));

        // Remove each label the message already carries.
        for (i, l) in m.labels.iter().enumerate() {
            msg = msg.with_action(immediate_spec(
                &format!("remove-label-{i}"),
                &format!("Remove label “{l}”"),
                &remove_label_action(vec![m.id.clone()], l),
            ));
        }

        folder_node = folder_node.with_child(msg);
    }
    root = root.with_child(folder_node);

    // User-defined local folders — rename / delete.
    for (id, name) in &ctx.custom_folders {
        let rid = id.clone();
        let mut f = Resource::new(format!("mail.folder:{id}"), "mail.folder", name.clone());
        f = f.with_action(
            ActionSpec::prompted(
                "rename",
                "Rename folder",
                "Name",
                "folder name…",
                ExecutionLocus::ClientLocal,
                move |n: &str| encode(&rename_folder_action(&rid, n.trim())),
            )
            .reversible(),
        );
        f = f.with_action(
            immediate_spec("delete-folder", "Delete folder", &delete_folder_action(id))
                .destructive(),
        );
        root = root.with_child(f);
    }

    vec![root]
}

impl ResourceProvider for crate::app::MailApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.mail_resource_ctx(doc), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MailApp, avatar_store_offline};
    use crate::model::Folder;
    use crate::state::{CtxAction, MailState};

    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does.
    fn dispatch(doc: &mut AutoCommit, action: &MailAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A sample app synced into a fresh document, plus that document.
    fn seeded_app() -> (MailApp, ui::State) {
        let mut app = MailApp::new_sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        (app, state)
    }

    /// A sample message id (as a string) taken from the Inbox.
    fn an_inbox_id(st: &MailState) -> String {
        st.mailbox.messages_in(Folder::Inbox)[0].id.to_string()
    }

    /// Ctrl+K on Mail yields the mailbox root exporting "New folder", the open
    /// folder with `mail.message:<id>` children exporting read / star / archive /
    /// delete, and surfaces them in the one palette. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_mailbox_tree() {
        let (app, state) = seeded_app();
        let resources = Widget::resources(&app, &state.doc);
        assert_eq!(resources.len(), 1, "one mailbox root");
        let mailbox = &resources[0];
        assert_eq!(mailbox.kind, "mail.mailbox");
        let mb_actions: Vec<&str> = mailbox.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(mb_actions.contains(&"new-folder"), "{mb_actions:?}");
        assert!(mb_actions.contains(&"refresh"), "{mb_actions:?}");
        let refresh = mailbox.actions.iter().find(|a| a.id == "refresh").unwrap();
        assert_eq!(
            refresh.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(refresh.contract.result, qos_actions::ResultSpec::None);

        let folder = mailbox
            .children
            .iter()
            .find(|r| r.kind == "mail.folder")
            .expect("an open folder node");
        let message = folder
            .children
            .iter()
            .find(|r| r.kind == "mail.message")
            .expect("a message resource in the open folder");
        let msg_actions: Vec<&str> = message.actions.iter().map(|a| a.id.as_str()).collect();
        for want in ["read", "star", "archive", "delete", "add-label"] {
            assert!(
                msg_actions.contains(&want),
                "message exports {want}: {msg_actions:?}"
            );
        }
        assert_eq!(
            message
                .actions
                .iter()
                .find(|a| a.id == "delete")
                .unwrap()
                .contract
                .risk,
            qos_actions::RiskLevel::Destructive
        );

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New folder")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Archive")),
            "the palette surfaces a message action: {labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor a
    /// move-to-Trash mutates the document and records exactly one undo boundary,
    /// and Ctrl+Z (the host's `undo`) reverts precisely that action.
    #[test]
    fn move_action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        // A message currently in the Inbox.
        let id = "msg-1";
        MailDoc::set_folder_for(&mut state.doc, id, Folder::Inbox);
        let before_heads = state.doc.get_heads();
        assert_eq!(MailDoc::get_folder_for(&state.doc, id), Some(Folder::Inbox));

        let encoded = encode(&move_action(vec![id.to_string()], Folder::Trash));
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            MailDoc::get_folder_for(&state.doc, id),
            Some(Folder::Trash),
            "the action moved the message to Trash"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            MailDoc::get_folder_for(&state.doc, id),
            Some(Folder::Inbox),
            "undo reverted the move and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn set_starred_replays_byte_identical_on_a_peer() {
        let mut a = AutoCommit::new();
        MailDoc::set_folder_for(&mut a, "m", Folder::Inbox);
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_starred_action(vec!["m".to_string()], true);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(MailDoc::get_starred(&b, "m"), Some(true));
    }

    /// A batched multi-message move is one change and replays byte-identically on
    /// a peer — the whole batch, one actor, equal heads.
    #[test]
    fn batched_move_replays_byte_identical() {
        let mut a = AutoCommit::new();
        for id in ["m1", "m2", "m3"] {
            MailDoc::set_folder_for(&mut a, id, Folder::Inbox);
        }
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = move_action(vec!["m1".into(), "m2".into(), "m3".into()], Folder::Archive);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "the whole batch replayed identically"
        );
        for id in ["m1", "m2", "m3"] {
            assert_eq!(MailDoc::get_folder_for(&b, id), Some(Folder::Archive));
        }
    }

    /// Adding a label also replays byte-identically — the list-mutating path, not
    /// just a scalar flag.
    #[test]
    fn add_label_replays_byte_identical() {
        let mut a = AutoCommit::new();
        MailDoc::set_folder_for(&mut a, "m", Folder::Inbox);
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let action = add_label_action(vec!["m".to_string()], "work");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "label add replayed identically"
        );
        assert_eq!(MailDoc::get_labels(&b, "m"), vec!["work"]);
    }

    /// Saving a draft persists it; discarding clears it — both through the action.
    #[test]
    fn save_then_discard_draft_action() {
        let mut doc = AutoCommit::new();
        let draft = Draft {
            to: "x@y.example".into(),
            subject: "Hi".into(),
            ..Default::default()
        };
        dispatch(&mut doc, &save_draft_action(&draft));
        assert_eq!(MailDoc::load_draft(&doc).to, "x@y.example");
        dispatch(&mut doc, &discard_draft_action());
        assert!(
            MailDoc::load_draft(&doc).to.is_empty(),
            "discard cleared the draft"
        );
    }

    /// The "New folder" prompted action mints an id and creates a folder in the
    /// registry.
    #[test]
    fn new_folder_action_creates_a_local_folder() {
        let (app, state) = seeded_app();
        let resources = Widget::resources(&app, &state.doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-folder")
            .unwrap();
        let encoded = prompted_encode(spec, "Receipts");
        let mut doc = state.doc;
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        let folders = MailDoc::list_folders(&doc);
        assert_eq!(folders.len(), 1, "one local folder created");
        assert_eq!(folders[0].1, "Receipts");
    }

    /// Single source of truth #1: the palette's per-message archive / read specs
    /// encode byte-identically to the builders the on-screen controls call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, state) = seeded_app();
        let resources = Widget::resources(&app, &state.doc);
        let folder = resources[0]
            .children
            .iter()
            .find(|r| r.kind == "mail.folder")
            .unwrap();
        let message = folder
            .children
            .iter()
            .find(|r| r.kind == "mail.message")
            .unwrap();
        let id = message.id.0.trim_start_matches("mail.message:").to_string();

        // Archive spec == move-to-Archive builder.
        let archive_spec = message.actions.iter().find(|a| a.id == "archive").unwrap();
        let from_spec = immediate_encode(archive_spec);
        let from_builder = encode(&move_action(vec![id.clone()], Folder::Archive));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "archive is one source of truth"
        );

        // Read toggle spec == set-read builder for the negated current state.
        let read_spec = message.actions.iter().find(|a| a.id == "read").unwrap();
        let row_read = app
            .mail_resource_ctx(&state.doc)
            .messages
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.read)
            .unwrap();
        let from_spec = immediate_encode(read_spec);
        let from_builder = encode(&set_read_action(vec![id.clone()], !row_read));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "read toggle is one source of truth"
        );

        // Add-label prompted spec == add-label builder for the same text.
        let label_spec = message
            .actions
            .iter()
            .find(|a| a.id == "add-label")
            .unwrap();
        let from_spec = prompted_encode(label_spec, "work");
        let from_builder = encode(&add_label_action(vec![id], "work"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "add-label is one source of truth"
        );
    }

    /// Single source of truth #2: the on-screen message affordance
    /// ([`MailState::apply_message_action`]) dispatches the byte-identical action
    /// the palette's archive spec builds — proven on the real dispatch path via
    /// the framework [`ui::ActionHost`].
    #[test]
    fn affordance_dispatches_the_palette_action() {
        let mut st = MailState::new_sample(avatar_store_offline());
        let mut doc = AutoCommit::new();
        let id = an_inbox_id(&st);

        st.apply_message_action(&mut doc, id.parse().unwrap(), CtxAction::Archive);
        let dispatched = st
            .actions
            .last_dispatched()
            .expect("archive dispatched an action");
        let expected = encode(&move_action(vec![id.clone()], Folder::Archive));
        assert_eq!(dispatched.type_id, expected.type_id);
        assert_eq!(
            dispatched.payload, expected.payload,
            "the archive affordance and the palette build the identical action"
        );
        // And it really moved the message in the document.
        assert_eq!(MailDoc::get_folder_for(&doc, &id), Some(Folder::Archive));
    }
}
