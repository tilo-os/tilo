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
//! label, create/rename/delete a local folder, permanently purge a trashed
//! message, save/discard the draft, and every edit of the draft's body (rich
//! text, edited in the SDK rich-text editor).
//!
//! **External, non-deterministic → never an action:** IMAP fetch/sync, SMTP send,
//! and account auth all write mailbox state as *external ingestion* driven by the
//! network (see [`crate::engine`]); they can't replay byte-identically, so they
//! stay outside the action model. Sending a mail is an external effect; **saving**
//! a draft is an action.
//!
//! **Presentation / view state → never an action:** the compose header fields
//! until they are saved or sent, the selection, the search query, which
//! folder/pane is open, and the read-pane scroll. Nothing to replicate.
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
use tilo_document::RichTextAction;

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, PermissionRequirement,
    Resource, ResultSpec, RetryPolicy,
};

use crate::attachment_store;
use crate::doc::MailDoc;
use crate::model::{Draft, DraftAttachment, Folder};

/// Registry `type_id` for every Mail action.
pub const MAIL_ACTION_TYPE: &str = "mail.action";
pub const MAIL_OPERATION_TYPE: &str = "mail.operation";

/// Bounds applied at the replicated-action boundary.  The UI normally keeps
/// values much smaller; these limits make a malformed, remote, or replayed
/// envelope unable to turn one mail action into an unbounded CRDT delta.
const MAX_MESSAGE_IDS_PER_ACTION: usize = 256;
const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_LABEL_CHARS: usize = 128;
const MAX_FOLDER_NAME_CHARS: usize = 96;
const MAX_DRAFT_ADDRESS_CHARS: usize = 8 * 1024;
const MAX_DRAFT_SUBJECT_CHARS: usize = 998;
const MAX_DRAFT_BODY_CHARS: usize = 256 * 1024;
const MAX_CALENDAR_ICS_BYTES: usize = 256 * 1024;

/// Take at most `max` Unicode scalar values without slicing a UTF-8 byte
/// offset.  Control characters are removed from one-line fields; bodies keep
/// normal newlines but still reject NUL and other control injection.
fn bounded_text(value: &str, max: usize, multiline: bool) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control() || (multiline && matches!(ch, '\n' | '\r' | '\t')))
        .take(max)
        .collect()
}

/// Normalise a batch before applying it.  This both bounds work and makes a
/// duplicated id in a hostile payload behave exactly like one requested id.
fn bounded_ids(ids: &[String]) -> Vec<&str> {
    let mut result = Vec::with_capacity(ids.len().min(MAX_MESSAGE_IDS_PER_ACTION));
    for id in ids {
        if result.len() == MAX_MESSAGE_IDS_PER_ACTION {
            break;
        }
        let id = id.trim();
        if id.is_empty() || id.chars().count() > MAX_MESSAGE_ID_CHARS || result.contains(&id) {
            continue;
        }
        result.push(id);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailOperation {
    /// Start a new local composition.  The body is intentionally not part of
    /// this view operation: until it is saved as a draft or sent, a compose
    /// buffer is private presentation state rather than shared mailbox data.
    ComposeNew,
    RefreshCurrentFolder,
    /// Reveal a system folder without changing the collaborative document.
    RevealFolder {
        folder: String,
    },
    RevealAccount {
        /// Account ids come from the device-local account store, so an action
        /// address keeps pointing at the same account when another account is
        /// added, removed, or reordered.
        id: String,
    },
    /// Reveal an already-loaded message. The admitted Open operation
    /// synchronously dispatches the canonical document `SetRead` action before
    /// it completes, so history, sync, notifications, and unread counters all
    /// observe the same state transition. This is the operation behind the
    /// plainly named **Open message** action.
    RevealMessage {
        id: String,
    },
    /// Inspect an already-loaded message without changing its read flag.
    ///
    /// This deliberately has a different operation and a deliberately explicit
    /// label in the resource tree: an agent (or a person checking a message)
    /// must never get a surprising read receipt merely by asking to inspect it.
    /// Normal list activation and **Open message** still use [`Self::RevealMessage`]
    /// and therefore mark an unread message as read.
    InspectMessage {
        id: String,
    },
    /// Start a reply to one loaded message.  This is a view operation because
    /// it only opens a local, unsaved compose buffer; saving that buffer uses
    /// the canonical `SaveDraft` document action.
    Reply {
        id: String,
    },
    /// Start a reply-all composition for one loaded message.
    ReplyAll {
        id: String,
    },
    /// Start a forward composition for one loaded message.
    Forward {
        id: String,
    },
    RevealDraft,
    SaveAttachment {
        message_id: String,
        index: usize,
    },
    /// Validate bounded RFC 5545 invitation content, save it inline in a
    /// durable draft through `MailAction`, and reveal it. No host path is
    /// accepted or inferred.
    PrepareCalendarInvitation {
        to: String,
        subject: String,
        body: String,
        calendar_ics: String,
    },
}

/// Decode a Mail effect operation when `type_id` addresses this application.
///
/// # Errors
///
/// Returns the JSON decoding error when a Mail operation payload is malformed.
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
    #[serde(default)]
    pub calendar_ics: Option<String>,
    /// Portable content-addressed attachment references. This is structured metadata, not
    /// a machine-local path, so collaborators can materialize the same bytes.
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
}

impl DraftData {
    pub fn from_draft(d: &Draft) -> Self {
        Self {
            to: bounded_text(&d.to, MAX_DRAFT_ADDRESS_CHARS, false),
            cc: bounded_text(&d.cc, MAX_DRAFT_ADDRESS_CHARS, false),
            bcc: bounded_text(&d.bcc, MAX_DRAFT_ADDRESS_CHARS, false),
            subject: bounded_text(&d.subject, MAX_DRAFT_SUBJECT_CHARS, false),
            body: bounded_text(&d.body, MAX_DRAFT_BODY_CHARS, true),
            html_body: d
                .html_body
                .as_deref()
                .map(|body| bounded_text(body, MAX_DRAFT_BODY_CHARS, true)),
            calendar_ics: d
                .calendar_ics
                .as_deref()
                .filter(|value| valid_calendar_invitation(value))
                .map(ToOwned::to_owned),
            attachments: attachment_store::bounded(&d.attachments),
        }
    }

    fn to_draft(&self) -> Draft {
        Draft {
            to: bounded_text(&self.to, MAX_DRAFT_ADDRESS_CHARS, false),
            cc: bounded_text(&self.cc, MAX_DRAFT_ADDRESS_CHARS, false),
            bcc: bounded_text(&self.bcc, MAX_DRAFT_ADDRESS_CHARS, false),
            subject: bounded_text(&self.subject, MAX_DRAFT_SUBJECT_CHARS, false),
            body: bounded_text(&self.body, MAX_DRAFT_BODY_CHARS, true),
            html_body: self
                .html_body
                .as_deref()
                .map(|body| bounded_text(body, MAX_DRAFT_BODY_CHARS, true)),
            calendar_ics: self
                .calendar_ics
                .as_deref()
                .filter(|value| valid_calendar_invitation(value))
                .map(ToOwned::to_owned),
            attachments: attachment_store::bounded(&self.attachments),
            in_reply_to: None,
        }
    }
}

/// Whether `value` is a bounded iTIP `REQUEST` Mail may attach: one CRLF
/// `VCALENDAR` whose components nest correctly, with `METHOD:REQUEST` on the
/// calendar and at least one `VEVENT`, every one carrying the same `UID` (a
/// recurring event's overridden occurrences are further `VEVENT`s of it).
///
/// The document is parsed as content lines, so text that merely looks like
/// structure (`SUMMARY:BEGIN:VEVENT`, a folded `METHOD:REQUEST` inside an
/// event) cannot pass or fail the check.
pub fn valid_calendar_invitation(value: &str) -> bool {
    if value.len() > MAX_CALENDAR_ICS_BYTES || value.contains('\0') {
        return false;
    }
    let Some(lines) = pim_lines::parse_strict(value) else {
        return false;
    };
    let mut stack: Vec<String> = Vec::new();
    let mut calendars = 0_usize;
    let mut method_request = false;
    let mut events = 0_usize;
    let mut event_uid: Option<String> = None;
    let mut shared_uid: Option<String> = None;
    for line in &lines {
        let component = line.value.trim().to_ascii_uppercase();
        match line.name.as_str() {
            "BEGIN" => {
                if component.is_empty() {
                    return false;
                }
                match (stack.len(), component.as_str()) {
                    (0, "VCALENDAR") => calendars += 1,
                    (0, _) => return false,
                    (1, "VEVENT") => {
                        events += 1;
                        event_uid = None;
                    }
                    _ => {}
                }
                stack.push(component);
            }
            "END" => {
                if stack.pop().as_deref() != Some(component.as_str()) {
                    return false;
                }
                if stack.len() == 1 && component == "VEVENT" {
                    let Some(uid) = event_uid.take() else {
                        return false;
                    };
                    if shared_uid.get_or_insert_with(|| uid.clone()) != &uid {
                        return false;
                    }
                }
            }
            "METHOD" if stack.len() == 1 => {
                method_request = component == "REQUEST";
            }
            "UID" if stack.len() == 2 && stack.last().is_some_and(|top| top == "VEVENT") => {
                let uid = line.value.trim();
                if uid.is_empty() {
                    return false;
                }
                event_uid = Some(uid.to_owned());
            }
            _ if stack.is_empty() => return false,
            _ => {}
        }
    }
    stack.is_empty() && calendars == 1 && method_request && events >= 1
}

/// Every collaborative operation Mail exports on its shared mailbox state.
///
/// Determinism contract: ids and target values are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock and applies its ids
/// in the payload's given order. A malformed payload is a **no-op**.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MailAction {
    /// Set the read/unread flag on a batch of messages.
    SetRead { ids: Vec<String>, read: bool },
    /// Set the starred flag on a batch of messages.
    SetStarred { ids: Vec<String>, starred: bool },
    /// Move a batch of messages to `folder` (a folder key from
    /// [`Folder::as_key`]). Archive, delete-to-trash, junk, and move-to-inbox are
    /// all this with a specific target. An unknown key is a no-op.
    Move { ids: Vec<String>, folder: String },
    /// Permanently remove a batch which is already in Trash. This writes a
    /// tombstone rather than directly altering a peer's hydrated corpus, so a
    /// delayed action cannot resurrect its flags.
    Purge { ids: Vec<String> },
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
    /// Start the in-progress draft's body afresh: a blank message, a reply's
    /// quote, a reopened draft.
    StartDraftBody(DraftBodySeed),
    /// One edit of the in-progress draft's body.
    EditDraftBody(RichTextAction),
    /// First baseline for messages hydrated from the mail server. Existing
    /// flags win, so a later fetch cannot overwrite a `MailAction`.
    IngestBaselines { messages: Vec<IngestBaseline> },
}

/// A message id plus the server flags used only when the document has none yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestBaseline {
    pub id: String,
    pub read: Option<bool>,
    pub starred: Option<bool>,
    pub folder: Option<String>,
}

impl IngestBaseline {
    pub fn from_message(message: &crate::model::Message) -> Self {
        Self {
            id: message.id.to_string(),
            read: Some(message.read),
            starred: Some(message.starred),
            folder: Some(message.folder.as_key().to_string()),
        }
    }

    pub fn register_only(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            read: None,
            starred: None,
            folder: None,
        }
    }
}

/// What a draft body starts from: the HTML part when there is one, else the
/// plain text, a paragraph per line (a `> ` line quoted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DraftBodySeed {
    pub text: String,
    pub html: Option<String>,
}

impl DraftBodySeed {
    /// A body of plain text.
    pub fn text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            html: None,
        }
    }

    /// The body a saved draft was written with.
    pub fn of_draft(draft: &Draft) -> Self {
        Self {
            text: draft.body.clone(),
            html: draft.html_body.clone(),
        }
    }

    fn bounded(&self) -> Self {
        Self {
            text: bounded_text(&self.text, MAX_DRAFT_BODY_CHARS, true),
            html: self
                .html
                .as_deref()
                .map(|html| bounded_text(html, MAX_DRAFT_BODY_CHARS, true)),
        }
    }
}

impl Action for MailAction {
    #[allow(
        clippy::too_many_lines,
        reason = "mail CRDT mutations stay in one Action::apply match"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            MailAction::SetRead { ids, read } => {
                for id in bounded_ids(ids) {
                    if MailDoc::is_present_message(doc, id) {
                        MailDoc::set_read(doc, id, *read);
                    }
                }
            }
            MailAction::SetStarred { ids, starred } => {
                for id in bounded_ids(ids) {
                    if MailDoc::is_present_message(doc, id) {
                        MailDoc::set_starred(doc, id, *starred);
                    }
                }
            }
            MailAction::Move { ids, folder } => {
                // An unknown folder key (e.g. a stale local-folder id) is a no-op
                // rather than a wrong move — the system `Folder` enum is total.
                if let Some(f) = Folder::from_key(folder) {
                    for id in bounded_ids(ids) {
                        if MailDoc::is_present_message(doc, id) {
                            MailDoc::set_folder_for(doc, id, f);
                        }
                    }
                }
            }
            MailAction::Purge { ids } => {
                let ids = bounded_ids(ids);
                // Purge is all-or-nothing. A stale search result or a message
                // concurrently moved out of Trash must refuse the whole batch,
                // rather than permanently delete whatever happens to remain
                // valid. This preserves the atomic contract agents and the
                // selection toolbar publish.
                if ids.is_empty()
                    || ids.iter().any(|id| {
                        !MailDoc::is_present_message(doc, id)
                            || MailDoc::get_folder_for(doc, id) != Some(Folder::Trash)
                    })
                {
                    return Err(ActionError::Refused(
                        "every message must still be present in Trash before permanent deletion"
                            .into(),
                    ));
                }
                for id in ids {
                    MailDoc::mark_message_purged(doc, id);
                }
            }
            MailAction::AddLabel { ids, label } => {
                let label = bounded_text(label, MAX_LABEL_CHARS, false);
                if label.is_empty() {
                    return Ok(());
                }
                for id in bounded_ids(ids) {
                    if MailDoc::is_present_message(doc, id) {
                        MailDoc::add_label(doc, id, &label);
                    }
                }
            }
            MailAction::RemoveLabel { ids, label } => {
                let label = bounded_text(label, MAX_LABEL_CHARS, false);
                if label.is_empty() {
                    return Ok(());
                }
                for id in bounded_ids(ids) {
                    if MailDoc::is_present_message(doc, id) {
                        MailDoc::remove_label(doc, id, &label);
                    }
                }
            }
            MailAction::CreateFolder { id, name } => {
                let name = bounded_text(name, MAX_FOLDER_NAME_CHARS, false);
                if id.chars().count() <= MAX_MESSAGE_ID_CHARS && !name.is_empty() {
                    MailDoc::create_folder(doc, id, &name);
                }
            }
            MailAction::RenameFolder { id, name } => {
                let name = bounded_text(name, MAX_FOLDER_NAME_CHARS, false);
                if id.chars().count() <= MAX_MESSAGE_ID_CHARS && !name.is_empty() {
                    MailDoc::rename_folder(doc, id, &name);
                }
            }
            MailAction::DeleteFolder { id } => {
                if id.chars().count() <= MAX_MESSAGE_ID_CHARS {
                    MailDoc::delete_folder(doc, id);
                }
            }
            MailAction::SaveDraft(data) => {
                MailDoc::store_draft(doc, &data.to_draft());
            }
            MailAction::DiscardDraft => {
                MailDoc::clear_draft(doc);
            }
            MailAction::StartDraftBody(seed) => MailDoc::start_draft_body(doc, &seed.bounded()),
            MailAction::EditDraftBody(edit) => {
                let body = MailDoc::draft_body(doc)
                    .ok_or_else(|| ActionError::Refused("the draft has no body".into()))?;
                edit.apply_under(doc, &body)?;
            }
            MailAction::IngestBaselines { messages } => {
                for message in messages {
                    let folder = message.folder.as_deref().and_then(Folder::from_key);
                    MailDoc::ingest_baseline(
                        doc,
                        &message.id,
                        message.read,
                        message.starred,
                        folder,
                    );
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// Whether a message was read is state about it, not a change to it.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            MailAction::SetRead { .. } => tilo_actions::ChangeKind::Housekeeping,
            _ => tilo_actions::ChangeKind::Content,
        }
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

/// Permanently purge a batch of already-trashed messages.
pub fn purge_action(ids: Vec<String>) -> MailAction {
    MailAction::Purge { ids }
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

pub fn prepare_calendar_invitation_operation(
    to: &str,
    subject: &str,
    body: &str,
    calendar_ics: &str,
) -> EncodedAction {
    EncodedAction::encode_operation(
        MAIL_OPERATION_TYPE,
        &MailOperation::PrepareCalendarInvitation {
            to: to.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            calendar_ics: calendar_ics.to_string(),
        },
    )
    .unwrap_or_else(|_| EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: b"Mail invitation handoff could not be encoded".to_vec(),
    })
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Largest number of message rows materialised into the currently-open
/// resource tree. Individual messages and threads remain lazily resolvable by
/// stable id, so this is a snapshot-memory ceiling rather than a discovery cap.
pub(crate) const OPEN_MESSAGE_LIMIT: usize = 100;

/// One message surfaced as a resource, with the scalar state its actions need
/// resolved at authoring time (so "Mark read" builds the byte-identical action
/// the on-screen control does).
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    /// Stable conversation anchor from [`crate::model::Thread::id`]. This is
    /// separate from the subject: display text is neither unique nor durable.
    pub thread_id: String,
    pub subject: String,
    pub thread_subject: String,
    pub folder: Folder,
    pub read: bool,
    pub starred: bool,
    pub labels: Vec<String>,
    pub attachments: Vec<AttachmentRow>,
}

#[derive(Debug, Clone)]
pub struct AttachmentRow {
    pub filename: String,
    pub size: u64,
    pub downloadable: bool,
}

/// Project exactly one loaded message into the scalar data required to author
/// its canonical resource. Keeping this conversion public within Mail lets the
/// small open-resource snapshot and lazy resolver use the same row without
/// cloning the entire mailbox.
pub(crate) fn message_row(message: &crate::model::Message) -> MessageRow {
    MessageRow {
        id: message.id.to_string(),
        // A caller that materialises a folder tree replaces this with the
        // thread projection's anchor. Keeping the message id here makes
        // on-demand message/search rows safe and self-contained.
        thread_id: message.id.to_string(),
        subject: message.subject.clone(),
        thread_subject: message.thread_subject(),
        folder: message.folder,
        read: message.read,
        starred: message.starred,
        labels: message.labels.clone(),
        attachments: message
            .attachments
            .iter()
            .map(|attachment| AttachmentRow {
                filename: attachment.filename.clone(),
                size: attachment.size,
                downloadable: attachment.is_downloadable(),
            })
            .collect(),
    }
}

/// The presentation context Mail hands [`provide_resources`]: the device-local
/// account list plus collaborative mailbox state (current folder/messages,
/// local-folder registry, and draft). Connection accounts stay out of the
/// Automerge document; their ids are nevertheless stable resource addresses.
#[derive(Debug, Clone)]
pub struct MailResourceCtx {
    pub mailbox_label: String,
    pub current_folder: Folder,
    pub accounts: Vec<(String, String, bool)>,
    /// Loaded messages in the currently-open folder. Resource materialization
    /// applies its own bound so a large mailbox cannot inflate Ctrl+K snapshots.
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

/// A bounded, typed mailbox-level batch action.  This is deliberately a
/// mailbox capability rather than an implementation detail of the visible
/// list: search and agents can therefore submit the same one-action atomic
/// triage operation with stable message ids.  The document action remains the
/// final defensive boundary; these checks make the published contract honest
/// before a payload is authored.
fn bulk_spec(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    reversible: bool,
    build: impl Fn(Vec<String>) -> MailAction + Send + Sync + 'static,
) -> ActionSpec {
    let spec = ActionSpec::typed(
        id,
        label,
        "message ids…",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("ids", ParamType::Array, description).with_schema(
                serde_json::json!({
                    "minItems": 1,
                    "maxItems": MAX_MESSAGE_IDS_PER_ACTION,
                    "items": {"type": "string", "minLength": 1, "maxLength": MAX_MESSAGE_ID_CHARS}
                }),
            ),
        ],
        move |args: &ActionArgs| {
            let ids = args
                .array("ids")
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .filter(|id| !id.is_empty() && id.chars().count() <= MAX_MESSAGE_ID_CHARS)
                .fold(Vec::new(), |mut ids, id| {
                    if ids.len() < MAX_MESSAGE_IDS_PER_ACTION && !ids.iter().any(|seen| seen == id)
                    {
                        ids.push(id.to_string());
                    }
                    ids
                });
            encode(&build(ids))
        },
    );
    if reversible { spec.reversible() } else { spec }
}

#[allow(
    clippy::expect_used,
    reason = "MailOperation is a derived-Serialize enum; an unencodable operation is a programmer invariant violation"
)]
fn view_spec(id: &str, label: &str, operation: &MailOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        MAIL_OPERATION_TYPE,
        operation,
    )
    .expect("encode mail view operation")
    .retry_policy(RetryPolicy::Idempotent)
}

#[allow(
    clippy::expect_used,
    reason = "MailOperation is a derived-Serialize enum; an unencodable operation is a programmer invariant violation"
)]
fn effect_spec(id: &str, label: &str, operation: &MailOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::Effect,
        MAIL_OPERATION_TYPE,
        operation,
    )
    .expect("encode mail effect operation")
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

pub(crate) fn message_resource(message: &MessageRow) -> Resource {
    let mut resource = Resource::new(
        format!("mail.message:{}", message.id),
        "mail.message",
        message_label(message),
    );
    resource = resource.with_action(view_spec(
        "open",
        "Open message",
        &MailOperation::RevealMessage {
            id: message.id.clone(),
        },
    ));
    resource = resource.with_action(view_spec(
        "inspect",
        "Preview without marking read",
        &MailOperation::InspectMessage {
            id: message.id.clone(),
        },
    ));
    // Composition is a first-class resource capability, rather than an
    // on-screen-only context-menu convenience.  Each operation opens exactly
    // the same local composer boundary as the reader buttons and shortcuts;
    // no unsaved recipient/body data enters the shared mailbox document.
    resource = resource.with_action(view_spec(
        "reply",
        "Reply",
        &MailOperation::Reply {
            id: message.id.clone(),
        },
    ));
    resource = resource.with_action(view_spec(
        "reply-all",
        "Reply all",
        &MailOperation::ReplyAll {
            id: message.id.clone(),
        },
    ));
    resource = resource.with_action(view_spec(
        "forward",
        "Forward",
        &MailOperation::Forward {
            id: message.id.clone(),
        },
    ));
    resource = resource.with_action(immediate_spec(
        "read",
        if message.read {
            "Mark unread"
        } else {
            "Mark read"
        },
        &set_read_action(vec![message.id.clone()], !message.read),
    ));
    resource = resource.with_action(immediate_spec(
        "star",
        if message.starred { "Unstar" } else { "Star" },
        &set_starred_action(vec![message.id.clone()], !message.starred),
    ));
    resource = resource.with_action(immediate_spec(
        "archive",
        "Archive",
        &move_action(vec![message.id.clone()], Folder::Archive),
    ));
    let delete = if message.folder == Folder::Trash {
        immediate_spec(
            "delete",
            "Delete permanently",
            &purge_action(vec![message.id.clone()]),
        )
    } else {
        immediate_spec(
            "delete",
            "Delete",
            &move_action(vec![message.id.clone()], Folder::Trash),
        )
    };
    resource = resource.with_action(delete.destructive());

    let message_id = message.id.clone();
    resource = resource.with_action(ActionSpec::prompted(
        "add-label",
        "Add label…",
        "Label",
        "label…",
        ExecutionLocus::ClientLocal,
        move |label: &str| encode(&add_label_action(vec![message_id.clone()], label.trim())),
    ));
    for (index, label) in message.labels.iter().enumerate() {
        resource = resource.with_action(immediate_spec(
            &format!("remove-label-{index}"),
            &format!("Remove label “{label}”"),
            &remove_label_action(vec![message.id.clone()], label),
        ));
    }
    for (index, attachment) in message.attachments.iter().enumerate() {
        resource = resource.with_child(attachment_resource(&message.id, index, attachment));
    }
    resource
}

fn attachment_resource(message_id: &str, index: usize, attachment: &AttachmentRow) -> Resource {
    let save = effect_spec(
        "save",
        "Save attachment",
        &MailOperation::SaveAttachment {
            message_id: message_id.to_string(),
            index,
        },
    )
    .with_contract(ActionContract {
        // A chosen destination can overwrite a user file. Retrying after a
        // lost terminal acknowledgement must not silently repeat that effect.
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Resource {
            resource_kind: "mail.attachment".into(),
        },
        tags: vec!["mail".into(), "attachment".into(), "file-save".into()],
        ..ActionContract::default()
    });
    Resource::new(
        format!("mail.attachment:{message_id}:{index}"),
        "mail.attachment",
        format!(
            "{} · {}",
            attachment.filename,
            ui::format::bytes(attachment.size)
        ),
    )
    .with_action(if attachment.downloadable {
        save
    } else {
        save.unavailable("not_downloaded", "Attachment content is not downloaded yet")
    })
}

/// Compose, refresh, and calendar invitation. [`tilo_resource::publish`] keeps
/// these Views and Effects. Account availability is overlaid afterwards.
pub(crate) fn mailbox_local_operations() -> Vec<ActionSpec> {
    let mut specs = vec![view_spec(
        "compose",
        "Compose message",
        &MailOperation::ComposeNew,
    )];
    if let Ok(refresh) = ActionSpec::operation(
        "refresh",
        "Refresh current folder",
        OperationKind::Effect,
        MAIL_OPERATION_TYPE,
        &MailOperation::RefreshCurrentFolder,
    ) {
        specs.push(refresh.with_contract(ActionContract {
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            tags: vec!["network".into(), "mail".into(), "refresh".into()],
            ..ActionContract::default()
        }));
    }
    specs.push(
        ActionSpec::typed_operation(
            "prepare-calendar-invitation",
            "Prepare calendar invitation",
            "recipient, subject and calendar payload",
            OperationKind::Effect,
            MAIL_OPERATION_TYPE,
            vec![
                ActionParam::required("to", ParamType::Text, "Invitation recipient"),
                ActionParam::required("subject", ParamType::Text, "Invitation subject"),
                ActionParam::required("body", ParamType::Text, "Plain-text invitation message"),
                ActionParam::required(
                    "calendar_ics",
                    ParamType::Text,
                    "Bounded text/calendar METHOD:REQUEST payload",
                ),
            ],
            |args| {
                prepare_calendar_invitation_operation(
                    args.text("to").unwrap_or_default(),
                    args.text("subject").unwrap_or_default(),
                    args.text("body").unwrap_or_default(),
                    args.text("calendar_ics").unwrap_or_default(),
                )
            },
        )
        .with_contract(ActionContract {
            permission: PermissionRequirement::Invite,
            retry: RetryPolicy::Idempotent,
            result: ResultSpec::Resource {
                resource_kind: "mail.draft".into(),
            },
            tags: vec![
                "mail".into(),
                "calendar".into(),
                "invitation".into(),
                "device-local-sensitive".into(),
            ],
            ..ActionContract::default()
        }),
    );
    specs
}

fn overlay_account_availability(root: &mut Resource, can_sync: bool) {
    if can_sync {
        return;
    }
    for (id, reason) in [
        ("refresh", "No connected mail account is available"),
        (
            "prepare-calendar-invitation",
            "Connect a Mail account before preparing an invitation",
        ),
    ] {
        if let Some(index) = root.actions.iter().position(|action| action.id == id) {
            let spec = root.actions.remove(index);
            root.actions
                .insert(index, spec.unavailable("account_offline", reason));
        }
    }
}

fn accounts_collection(ctx: &MailResourceCtx) -> Resource {
    Resource::new("mail.accounts", "mail.account.collection", "Accounts").with_children(
        ctx.accounts.iter().map(|(id, label, active)| {
            Resource::new(
                format!("mail.account:{id}"),
                "mail.account",
                if *active {
                    format!("{label} · Active")
                } else {
                    label.clone()
                },
            )
            .with_action(view_spec(
                "open",
                "Open account",
                &MailOperation::RevealAccount { id: id.clone() },
            ))
        }),
    )
}

/// Threads for the folder the user has open. `ctx.messages` is already that
/// folder's projection, including virtual Starred, so it is not filtered again.
/// A message [`tilo_resource::publish`] already listed keeps that document
/// action; open, reply, and attachments are added beside it.
fn current_folder_threads(
    folder: Folder,
    ctx: &MailResourceCtx,
    published_messages: &[Resource],
) -> Vec<Resource> {
    let mut threads: Vec<(String, String, Vec<&MessageRow>)> = Vec::new();
    for message in ctx.messages.iter().rev().take(OPEN_MESSAGE_LIMIT) {
        if let Some((_, _, messages)) = threads
            .iter_mut()
            .find(|(id, _, _)| id == &message.thread_id)
        {
            messages.push(message);
        } else {
            threads.push((
                message.thread_id.clone(),
                message.thread_subject.clone(),
                vec![message],
            ));
        }
    }
    threads
        .into_iter()
        .map(|(thread_id, thread_subject, messages)| {
            let stable = thread_resource_key(folder, &thread_id);
            Resource::new(
                format!("mail.thread:{stable}"),
                "mail.thread",
                if thread_subject.is_empty() {
                    "(no subject)".into()
                } else {
                    thread_subject
                },
            )
            .with_children(
                messages
                    .into_iter()
                    .map(|message| overlay_published_message(message, published_messages)),
            )
        })
        .collect()
}

/// The thread row for one message. The published message is the row; open,
/// reply, and attachments are added when that row does not already have them.
fn overlay_published_message(message: &MessageRow, published_messages: &[Resource]) -> Resource {
    let live = message_resource(message);
    let id = format!("mail.message:{}", message.id);
    let Some(published) = published_messages
        .iter()
        .find(|child| child.id.as_str() == id)
    else {
        return live;
    };
    let mut resource = published.clone();
    for action in live.actions {
        if !resource.actions.iter().any(|have| have.id == action.id) {
            resource.actions.push(action);
        }
    }
    for child in live.children {
        if !resource.children.iter().any(|have| have.id == child.id) {
            resource.children.push(child);
        }
    }
    resource
}

fn system_folder_key(id: &str) -> Option<Folder> {
    let key = id.strip_prefix("mail.folder:")?;
    Folder::ALL
        .into_iter()
        .find(|folder| folder.as_key() == key)
}

/// Open, and the draft affordances, on a folder [`tilo_resource::publish`]
/// already emitted. The open folder's children become the live thread page.
fn overlay_published_folder(folder_node: &mut Resource, folder: Folder, ctx: &MailResourceCtx) {
    if !folder_node.actions.iter().any(|action| action.id == "open") {
        folder_node.actions.push(view_spec(
            "open",
            "Open folder",
            &MailOperation::RevealFolder {
                folder: folder.as_key().to_string(),
            },
        ));
    }
    if folder == Folder::Drafts && ctx.has_draft {
        if !folder_node
            .actions
            .iter()
            .any(|action| action.id == "discard-draft")
        {
            folder_node.actions.push(
                immediate_spec("discard-draft", "Discard draft", &discard_draft_action())
                    .destructive(),
            );
        }
        if !folder_node
            .children
            .iter()
            .any(|child| child.id.as_str() == "mail.draft:current")
        {
            folder_node.children.push(
                Resource::new("mail.draft:current", "mail.draft", "Current draft")
                    .with_action(view_spec(
                        "open",
                        "Continue editing",
                        &MailOperation::RevealDraft,
                    ))
                    .with_action(
                        immediate_spec("discard", "Discard draft", &discard_draft_action())
                            .destructive(),
                    ),
            );
        }
    }
    if folder == ctx.current_folder {
        let draft = folder_node
            .children
            .iter()
            .find(|child| child.id.as_str() == "mail.draft:current")
            .cloned();
        let published_messages: Vec<Resource> = folder_node
            .children
            .iter()
            .filter(|child| child.kind == "mail.message")
            .cloned()
            .collect();
        folder_node.children = current_folder_threads(folder, ctx, &published_messages);
        if let Some(draft) = draft {
            folder_node.children.insert(0, draft);
        }
    }
}

pub fn provide_resources(ctx: &MailResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let state =
        <crate::resource::Mail as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
    let mut root = ui::tilo_resource::publish::<crate::resource::Mail>(
        "mail.mailbox",
        &ctx.mailbox_label,
        &state,
    );
    overlay_account_availability(&mut root, ctx.can_sync);
    // Accounts are the signed-in session, not mailbox document rows.
    if !root
        .children
        .iter()
        .any(|child| child.id.as_str() == "mail.accounts")
    {
        root.children.insert(0, accounts_collection(ctx));
    }
    for child in &mut root.children {
        if let Some(folder) = system_folder_key(child.id.as_str()) {
            overlay_published_folder(child, folder, ctx);
        }
    }
    // A custom folder the live context knows and the document has not published.
    for (id, name) in &ctx.custom_folders {
        let resource_id = format!("mail.folder:{id}");
        if root
            .children
            .iter()
            .any(|child| child.id.as_str() == resource_id)
        {
            continue;
        }
        root.children.push(publish_custom_folder_document(id, name));
    }
    vec![root]
}

/// Build the canonical resource for a built-in folder. Kept separate from the
/// tree assembly above so the search provider can materialize the *same*
/// folder resource lazily instead of returning an address which only happens
/// to work when another provider has already built the whole mailbox tree.
pub(crate) fn system_folder_resource(folder: Folder, has_draft: bool) -> Resource {
    let mut folder_node = Resource::new(
        format!("mail.folder:{}", folder.as_key()),
        "mail.folder",
        folder.title().to_string(),
    )
    .with_action(view_spec(
        "open",
        "Open folder",
        &MailOperation::RevealFolder {
            folder: folder.as_key().to_string(),
        },
    ));
    if folder == Folder::Drafts && has_draft {
        folder_node = folder_node.with_action(
            immediate_spec("discard-draft", "Discard draft", &discard_draft_action()).destructive(),
        );
        folder_node = folder_node.with_child(
            Resource::new("mail.draft:current", "mail.draft", "Current draft")
                .with_action(view_spec(
                    "open",
                    "Continue editing",
                    &MailOperation::RevealDraft,
                ))
                .with_action(
                    immediate_spec("discard", "Discard draft", &discard_draft_action())
                        .destructive(),
                ),
        );
    }
    folder_node
}

/// Mailbox-level Document ops for [`tilo_resource::publish`] — bulk triage and
/// new-folder. Compose, refresh, and the invitation are Views and Effects on
/// [`mailbox_local_operations`].
pub(crate) fn mailbox_bound_document_actions() -> Vec<ActionSpec> {
    vec![
        bulk_spec(
            "archive-messages",
            "Archive messages",
            "Stable ids of loaded messages to archive together",
            true,
            |ids| move_action(ids, Folder::Archive),
        ),
        bulk_spec(
            "move-messages-to-trash",
            "Move messages to Trash",
            "Stable ids of loaded messages to move to Trash together",
            true,
            |ids| move_action(ids, Folder::Trash),
        ),
        bulk_spec(
            "mark-messages-read",
            "Mark messages read",
            "Stable ids of loaded messages to mark read together",
            true,
            |ids| set_read_action(ids, true),
        ),
        bulk_spec(
            "mark-messages-unread",
            "Mark messages unread",
            "Stable ids of loaded messages to mark unread together",
            true,
            |ids| set_read_action(ids, false),
        ),
        bulk_spec(
            "purge-trashed-messages",
            "Delete trashed messages permanently",
            "Stable ids of messages already in Trash to delete permanently",
            false,
            purge_action,
        )
        .destructive(),
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
    ]
}

/// Message Document ops for [`tilo_resource::publish`] — read / star / archive /
/// delete / labels. Open / reply / forward / save-attachment stay Views/Effects.
pub(crate) fn publish_message_document(message: &MessageRow) -> Resource {
    let mut resource = Resource::new(
        format!("mail.message:{}", message.id),
        "mail.message",
        message_label(message),
    );
    resource = resource.with_action(immediate_spec(
        "read",
        if message.read {
            "Mark unread"
        } else {
            "Mark read"
        },
        &set_read_action(vec![message.id.clone()], !message.read),
    ));
    resource = resource.with_action(immediate_spec(
        "star",
        if message.starred { "Unstar" } else { "Star" },
        &set_starred_action(vec![message.id.clone()], !message.starred),
    ));
    resource = resource.with_action(immediate_spec(
        "archive",
        "Archive",
        &move_action(vec![message.id.clone()], Folder::Archive),
    ));
    let delete = if message.folder == Folder::Trash {
        immediate_spec(
            "delete",
            "Delete permanently",
            &purge_action(vec![message.id.clone()]),
        )
    } else {
        immediate_spec(
            "delete",
            "Delete",
            &move_action(vec![message.id.clone()], Folder::Trash),
        )
    };
    resource = resource.with_action(delete.destructive());

    let message_id = message.id.clone();
    resource = resource.with_action(ActionSpec::prompted(
        "add-label",
        "Add label…",
        "Label",
        "label…",
        ExecutionLocus::ClientLocal,
        move |label: &str| encode(&add_label_action(vec![message_id.clone()], label.trim())),
    ));
    for (index, label) in message.labels.iter().enumerate() {
        resource = resource.with_action(immediate_spec(
            &format!("remove-label-{index}"),
            &format!("Remove label “{label}”"),
            &remove_label_action(vec![message.id.clone()], label),
        ));
    }
    resource
}

fn publish_system_folder_document(folder: Folder, has_draft: bool) -> Resource {
    let mut folder_node = Resource::new(
        format!("mail.folder:{}", folder.as_key()),
        "mail.folder",
        folder.title().to_string(),
    );
    if folder == Folder::Drafts && has_draft {
        folder_node = folder_node.with_action(
            immediate_spec("discard-draft", "Discard draft", &discard_draft_action()).destructive(),
        );
        folder_node = folder_node.with_child(
            Resource::new("mail.draft:current", "mail.draft", "Current draft").with_action(
                immediate_spec("discard", "Discard draft", &discard_draft_action()).destructive(),
            ),
        );
    }
    folder_node
}

fn publish_custom_folder_document(id: &str, name: &str) -> Resource {
    let rid = id.to_owned();
    Resource::new(format!("mail.folder:{id}"), "mail.folder", name.to_owned())
        .with_action(
            ActionSpec::prompted(
                "rename",
                "Rename folder",
                "Name",
                "folder name…",
                ExecutionLocus::ClientLocal,
                move |n: &str| encode(&rename_folder_action(&rid, n.trim())),
            )
            .reversible(),
        )
        .with_action(
            immediate_spec("delete-folder", "Delete folder", &delete_folder_action(id))
                .destructive(),
        )
}

/// Folder / message Document children for [`tilo_resource::publish`]. Bounded
/// like the standalone eager page; subjects stay on the contextual tree.
pub(crate) fn publish_document_children(state: &crate::resource::MailboxState) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut remaining = OPEN_MESSAGE_LIMIT;
    for folder in Folder::ALL {
        let mut folder_node = publish_system_folder_document(folder, state.has_draft);
        if remaining > 0 {
            let folder_messages: Vec<&MessageRow> = state
                .messages
                .iter()
                .filter(|message| message.folder == folder)
                .take(remaining)
                .collect();
            for message in folder_messages {
                folder_node = folder_node.with_child(publish_message_document(message));
                remaining = remaining.saturating_sub(1);
            }
        }
        children.push(folder_node);
    }
    for (id, name) in &state.custom_folders {
        children.push(publish_custom_folder_document(id, name));
    }
    children
}

pub(crate) fn thread_resource_key(folder: Folder, thread_id: &str) -> String {
    // The RFC References root (or the model's documented legacy fallback) is
    // a stable domain identity. Scoping it to the folder prevents virtual
    // Starred and a physical folder from claiming the same resource address,
    // while avoiding lossy subject-derived hashes.
    format!("{}:{thread_id}", folder.as_key())
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::MailApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut roots = provide_resources(&self.mail_resource_ctx(doc), doc);
        if let Some(error) = self.mail_search_error()
            && let Some(mailbox) = roots
                .iter_mut()
                .find(|resource| resource.id.as_str() == "mail.mailbox")
        {
            mailbox.children.push(Resource::new(
                "mail.search-status",
                "mail.search_status",
                format!("Search index unavailable: {error}"),
            ));
        }
        roots
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        fn find(resources: Vec<Resource>, id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource);
                }
                if let Some(found) = find(resource.children, id) {
                    return Some(found);
                }
            }
            None
        }

        if id.starts_with("mail.message:")
            || id.starts_with("mail.attachment:")
            || id.starts_with("mail.thread:")
        {
            // A row already on the published mailbox keeps that document
            // action. The builders below cover a message the open page did
            // not materialise.
            if let Some(found) = find(self.publish_resources(doc), id) {
                return Some(found);
            }
        }
        if let Some(message_id) = id.strip_prefix("mail.message:") {
            let row = self.resource_message_row(message_id)?;
            return Some(message_resource(&row));
        }
        if let Some(rest) = id.strip_prefix("mail.attachment:")
            && let Some((message_id, index)) = rest.rsplit_once(':')
            && let Ok(index) = index.parse::<usize>()
        {
            let row = self.resource_message_row(message_id)?;
            return message_resource(&row).children.into_iter().nth(index);
        }
        if let Some(stable) = id.strip_prefix("mail.thread:") {
            let (subject, messages) = self.resource_thread_rows(stable)?;
            return Some(
                Resource::new(
                    id,
                    "mail.thread",
                    if subject.is_empty() {
                        "(no subject)".into()
                    } else {
                        subject
                    },
                )
                .with_children(messages.iter().map(message_resource)),
            );
        }
        if id == "mail.search-status" {
            return self.mail_search_error().map(|error| {
                Resource::new(
                    "mail.search-status",
                    "mail.search_status",
                    format!("Search index unavailable: {error}"),
                )
            });
        }
        let ctx = self.mail_resource_ctx(doc);
        find(provide_resources(&ctx, doc), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MailApp, avatar_store_offline};
    use crate::model::Folder;
    use crate::state::{CtxAction, MailState};
    use tilo_automerge::ApplyIncoming;

    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn invitation(events: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Calendar//EN\r\nMETHOD:REQUEST\r\n{events}END:VCALENDAR\r\n"
        )
    }

    const EVENT: &str = "BEGIN:VEVENT\r\nUID:a@tilo\r\nSUMMARY:Review\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n";

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn calendar_invitation_acceptance_is_structural_and_honours_overrides() {
        // Phase `calendar_invitation_accepts_a_request_and_its_overrides`.
        {
            assert!(valid_calendar_invitation(&invitation(EVENT)));
            let override_event = "BEGIN:VEVENT\r\nUID:a@tilo\r\nRECURRENCE-ID;VALUE=DATE:20260619\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n";
            assert!(
                valid_calendar_invitation(&invitation(&format!("{EVENT}{override_event}"))),
                "an overridden occurrence of the same event is part of one invitation"
            );
            let folded = invitation(EVENT).replace("SUMMARY:Review", "SUMMARY:Rev\r\n iew");
            assert!(valid_calendar_invitation(&folded));
        }

        // Phase `calendar_invitation_is_checked_structurally_not_textually`.
        {
            let lookalike = EVENT.replace("SUMMARY:Review", "SUMMARY:BEGIN:VEVENT");
            assert!(
                valid_calendar_invitation(&invitation(&lookalike)),
                "event text that looks like a component is just text"
            );
            let two_events = format!("{EVENT}{}", EVENT.replace("UID:a@tilo", "UID:b@tilo"));
            assert!(!valid_calendar_invitation(&invitation(&two_events)));
            let method_in_event = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nMETHOD:REQUEST\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            assert!(!valid_calendar_invitation(method_in_event));
            let publish = invitation(EVENT).replace("METHOD:REQUEST", "METHOD:PUBLISH");
            assert!(!valid_calendar_invitation(&publish));
            let no_uid = invitation(&EVENT.replace("UID:a@tilo\r\n", ""));
            assert!(!valid_calendar_invitation(&no_uid));
            let misnested = invitation(&EVENT.replace("END:VALARM", "END:VEVENT"));
            assert!(!valid_calendar_invitation(&misnested));
            let trailing = format!("{}BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", invitation(EVENT));
            assert!(!valid_calendar_invitation(&trailing));
            assert!(!valid_calendar_invitation(
                &invitation(EVENT).replace("\r\n", "\n")
            ));
            assert!(!valid_calendar_invitation(&invitation("")));
            let oversized = invitation(&format!(
                "BEGIN:VEVENT\r\nUID:a\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\n",
                "x".repeat(MAX_CALENDAR_ICS_BYTES)
            ));
            assert!(!valid_calendar_invitation(&oversized));
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
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
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A sample app synced into a fresh document, plus that document.
    fn seeded_app() -> (MailApp, ui::State) {
        let mut app = MailApp::new_sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        (app, state)
    }

    /// The mailbox resource tree: open vs neutral preview, bounded snapshots, stable
    /// thread/account addresses, visible failed refreshes, and one purge action for trash.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn mailbox_resources_publish_bounded_stable_addresses() {
        // Phase `message_resource_distinguishes_open_from_read_state_neutral_preview`.
        {
            let (app, state) = seeded_app();
            let message = app
                .mail_resource_ctx(&state.doc)
                .messages
                .into_iter()
                .find(|message| !message.read)
                .expect("sample has an unread message");
            let resource = message_resource(&message);
            let open = resource
                .actions
                .iter()
                .find(|action| action.id == "open")
                .expect("normal open action");
            let preview = resource
                .actions
                .iter()
                .find(|action| action.id == "inspect")
                .expect("explicit preview action");
            assert_eq!(open.label, "Open message");
            assert_eq!(preview.label, "Preview without marking read");
            for id in ["reply", "reply-all", "forward"] {
                assert!(
                    resource.actions.iter().any(|action| action.id == id),
                    "message resource exports {id} as a canonical compose action"
                );
            }
            let ActionInput::Immediate(open_payload) = &open.input else {
                panic!("open is an immediate view operation");
            };
            let ActionInput::Immediate(preview_payload) = &preview.input else {
                panic!("preview is an immediate view operation");
            };
            assert!(matches!(
                decode_operation(&open_payload.type_id, &open_payload.payload).unwrap(),
                Some(MailOperation::RevealMessage { .. })
            ));
            assert!(matches!(
                decode_operation(&preview_payload.type_id, &preview_payload.payload).unwrap(),
                Some(MailOperation::InspectMessage { .. })
            ));
        }

        // Phase `ctrl_k_publishes_the_mailbox_tree`.
        // Ctrl+K on Mail yields the mailbox root exporting "New folder", the open
        // folder with `mail.message:<id>` children exporting read / star / archive /
        // delete, and surfaces them in the one palette. Drives the real resource hook.
        {
            let (app, state) = seeded_app();
            let resources = ui::resources::resources(&app, &state.doc);
            let handlers: std::collections::HashSet<String> = app
                .local_resource_operation_handlers()
                .iter()
                .map(|handler| (*handler).to_string())
                .collect();
            let violations = tilo_actions::validate_surface(&resources, &registry(), &handlers);
            assert!(violations.is_empty(), "{violations:?}");
            assert_eq!(resources.len(), 1, "one mailbox root");
            let mailbox = &resources[0];
            assert_eq!(mailbox.kind, "mail.mailbox");
            let mb_actions: Vec<&str> = mailbox.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(mb_actions.contains(&"compose"), "{mb_actions:?}");
            assert!(mb_actions.contains(&"new-folder"), "{mb_actions:?}");
            assert!(mb_actions.contains(&"refresh"), "{mb_actions:?}");
            for id in [
                "archive-messages",
                "move-messages-to-trash",
                "mark-messages-read",
                "mark-messages-unread",
                "purge-trashed-messages",
            ] {
                assert!(mb_actions.contains(&id), "mailbox exposes typed batch {id}");
            }
            let refresh = mailbox.actions.iter().find(|a| a.id == "refresh").unwrap();
            assert_eq!(
                refresh.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(refresh.contract.result, tilo_actions::ResultSpec::Job);
            assert!(
                refresh
                    .contract
                    .result
                    .validate(&tilo_actions::ActionOutput::default())
                    .is_ok()
            );

            let folder = mailbox
                .children
                .iter()
                .find(|r| r.kind == "mail.folder")
                .expect("an open folder node");
            assert!(
                !folder.children.is_empty() && folder.children.len() <= 100,
                "the open folder publishes a bounded thread hierarchy"
            );
            assert!(
                folder.children.iter().all(|thread| {
                    thread.kind == "mail.thread"
                        && !thread.children.is_empty()
                        && thread
                            .children
                            .iter()
                            .all(|item| item.kind == "mail.message")
                }),
                "messages are grouped under searchable thread resources"
            );
            let id = app.mail_resource_ctx(&state.doc).messages[0].id.clone();
            let message =
                MailApp::publish_resource(&app, &state.doc, &format!("mail.message:{id}"))
                    .expect("a message remains addressable outside the snapshot");
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
                tilo_actions::RiskLevel::Destructive
            );
        }

        // Phase `failed_search_refresh_stays_visible_and_addressable_until_a_good_snapshot`.
        {
            let (app, state) = seeded_app();
            assert!(
                app.mail_search_error().is_none(),
                "the sample starts with a valid retained index"
            );

            app.inject_mail_search_snapshot_failure();
            let resources = ui::resources::resources(&app, &state.doc);
            let mailbox = resources
                .iter()
                .find(|resource| resource.id.as_str() == "mail.mailbox")
                .expect("mailbox root");
            assert!(
                mailbox
                    .children
                    .iter()
                    .any(|resource| resource.id.as_str() == "mail.search-status"),
                "the persistent index failure is published, not just shown in a toast"
            );
            let status = MailApp::publish_resource(&app, &state.doc, "mail.search-status")
                .expect("diagnostic resource resolves exactly");
            assert_eq!(status.kind, "mail.search_status");

            app.refresh_mail_search_for_test()
                .expect("a verified snapshot clears the old diagnostic");
            assert!(
                app.mail_search_error().is_none(),
                "only a successful replacement clears the retained error"
            );
            assert!(
                !ui::resources::resources(&app, &state.doc)
                    .iter()
                    .flat_map(|root| &root.children)
                    .any(|resource| resource.id.as_str() == "mail.search-status")
            );
        }

        // Phase `ten_thousand_messages_do_not_expand_the_open_snapshot`.
        {
            let mut ctx = MailResourceCtx {
                mailbox_label: "Scale".into(),
                current_folder: Folder::Inbox,
                accounts: vec![("scale@example.test".into(), "Scale".into(), true)],
                messages: Vec::new(),
                custom_folders: Vec::new(),
                has_draft: false,
                can_sync: false,
            };
            ctx.messages.extend((0..10_000).map(|id| MessageRow {
                id: id.to_string(),
                thread_id: id.to_string(),
                subject: format!("Message {id}"),
                thread_subject: format!("Message {id}"),
                folder: Folder::Inbox,
                read: false,
                starred: false,
                labels: Vec::new(),
                attachments: Vec::new(),
            }));
            let resources = provide_resources(&ctx, &AutoCommit::new());
            let inbox = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "mail.folder:inbox")
                .expect("inbox resource");
            assert_eq!(inbox.children.len(), 100, "open hierarchy is bounded");
            assert!(
                inbox
                    .children
                    .iter()
                    .all(|thread| { thread.kind == "mail.thread" && thread.children.len() == 1 })
            );
            assert!(message_resource(&ctx.messages[9_999]).actions.len() >= 5);
        }

        // Phase `same_subject_threads_keep_distinct_stable_resource_addresses`.
        {
            let ctx = MailResourceCtx {
                mailbox_label: "Mailbox".into(),
                current_folder: Folder::Inbox,
                accounts: Vec::new(),
                messages: vec![
                    MessageRow {
                        id: "newer".into(),
                        thread_id: "thread-b".into(),
                        subject: "Status update".into(),
                        thread_subject: "Status update".into(),
                        folder: Folder::Inbox,
                        read: false,
                        starred: false,
                        labels: Vec::new(),
                        attachments: Vec::new(),
                    },
                    MessageRow {
                        id: "older".into(),
                        thread_id: "thread-a".into(),
                        subject: "Status update".into(),
                        thread_subject: "Status update".into(),
                        folder: Folder::Inbox,
                        read: false,
                        starred: false,
                        labels: Vec::new(),
                        attachments: Vec::new(),
                    },
                ],
                custom_folders: Vec::new(),
                has_draft: false,
                can_sync: false,
            };
            let resources = provide_resources(&ctx, &AutoCommit::new());
            let inbox = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "mail.folder:inbox")
                .expect("inbox resource");
            assert_eq!(
                inbox.children.len(),
                2,
                "same display text must not merge threads"
            );
            let ids: Vec<_> = inbox
                .children
                .iter()
                .map(|thread| thread.id.as_str())
                .collect();
            assert!(ids.contains(&"mail.thread:inbox:thread-a"));
            assert!(ids.contains(&"mail.thread:inbox:thread-b"));
        }

        // Phase `account_resource_uses_a_durable_id_not_its_display_index`.
        {
            let ctx = MailResourceCtx {
                mailbox_label: "Mailbox".into(),
                current_folder: Folder::Inbox,
                accounts: vec![
                    ("personal-42".into(), "Personal".into(), true),
                    ("work-99".into(), "Work".into(), false),
                ],
                messages: Vec::new(),
                custom_folders: Vec::new(),
                has_draft: false,
                can_sync: false,
            };
            let resources = provide_resources(&ctx, &AutoCommit::new());
            let address = tilo_actions::ActionAddress::new("mail.account:work-99", "open");
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .expect("the stable account address resolves through the mailbox tree");
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(MailOperation::RevealAccount {
                    id: "work-99".into()
                })
            );
        }

        // Phase `trash_resource_uses_the_same_purge_action_as_the_delete_affordance`.
        {
            let row = MessageRow {
                id: "m".into(),
                thread_id: "m".into(),
                subject: "Already trashed".into(),
                thread_subject: "Already trashed".into(),
                folder: Folder::Trash,
                read: true,
                starred: false,
                labels: Vec::new(),
                attachments: Vec::new(),
            };
            let resource = message_resource(&row);
            let delete = resource
                .actions
                .iter()
                .find(|action| action.id == "delete")
                .expect("delete action");
            assert_eq!(delete.label, "Delete permanently");
            ui::testing::action_contract::assert_spec_encodes_as(
                delete,
                None,
                &encode(&purge_action(vec!["m".into()])),
            );
        }
    }

    /// A sample message id (as a string) taken from the Inbox.
    fn an_inbox_id(st: &MailState) -> String {
        st.mailbox.messages_in(Folder::Inbox)[0].id.to_string()
    }

    /// The sample app a contract seed was synced from, and the message the seed
    /// moved to Trash. The sample corpus mints fresh message ids per instance,
    /// so the resources must come from the very app that wrote the seed.
    struct Seeded {
        app: MailApp,
        trashed: String,
    }

    thread_local! {
        static SEEDED: std::cell::RefCell<Option<Seeded>> = const { std::cell::RefCell::new(None) };
    }

    fn with_seeded<R>(f: impl FnOnce(&Seeded) -> R) -> R {
        SEEDED.with_borrow(|seeded| f(seeded.as_ref().expect("the contract seed ran first")))
    }

    /// The sample mailbox after a user trashed one message, labelled another,
    /// created a local folder and started a draft — all through the actions.
    fn mailbox_seed() -> AutoCommit {
        let (mut app, mut state) = seeded_app();
        let ids = inbox_ids(&app, &state.doc);
        let trashed = ids.last().cloned().expect("sample inbox has messages");
        dispatch(
            &mut state.doc,
            &move_action(vec![trashed.clone()], Folder::Trash),
        );
        dispatch(
            &mut state.doc,
            &add_label_action(vec![ids[0].clone()], "work"),
        );
        dispatch(
            &mut state.doc,
            &create_folder_action("receipts", "Receipts"),
        );
        dispatch(
            &mut state.doc,
            &save_draft_action(&Draft {
                to: "ada@example.test".into(),
                subject: "Plans".into(),
                ..Default::default()
            }),
        );
        app.sync(&mut state.doc, &state.root);
        SEEDED.set(Some(Seeded { app, trashed }));
        state.doc
    }

    fn inbox_ids(app: &MailApp, doc: &AutoCommit) -> Vec<String> {
        app.mail_resource_ctx(doc)
            .messages
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    fn seeded_inbox_ids(doc: &AutoCommit) -> Vec<String> {
        with_seeded(|seeded| inbox_ids(&seeded.app, doc))
    }

    fn message_id(res: &Resource) -> &str {
        res.id
            .as_str()
            .strip_prefix("mail.message:")
            .expect("a message resource")
    }

    ui::action_contract_tests! {
        seed: mailbox_seed(),
        registry: registry(),
        resources: |doc| with_seeded(|seeded| seeded.app.publish_resources(doc)),
        palette: ["New folder", "Compose message", "Archive", "Rename folder", "Discard draft"],
        replay: [
            set_starred => |doc| {
                let id = seeded_inbox_ids(doc)
                    .into_iter()
                    .find(|id| MailDoc::get_starred(doc, id) == Some(false))
                    .expect("an unstarred inbox message");
                encode(&set_starred_action(vec![id], true))
            },
            batched_move => |doc| encode(&move_action(
                seeded_inbox_ids(doc).into_iter().take(3).collect(),
                Folder::Archive,
            )),
            add_label => |doc| encode(&add_label_action(vec![seeded_inbox_ids(doc)[1].clone()], "urgent")),
            purge => |_| encode(&purge_action(vec![with_seeded(|seeded| seeded.trashed.clone())])),
        ],
        undo: [
            move_to_trash => |doc| encode(&move_action(vec![seeded_inbox_ids(doc)[0].clone()], Folder::Trash)),
        ],
        encodes: [
            archive => ("mail.message", "archive", |_, res| {
                encode(&move_action(vec![message_id(res).to_string()], Folder::Archive))
            }),
            read => ("mail.message", "read", |doc, res| {
                let id = message_id(res);
                let read = MailDoc::get_read(doc, id).expect("an ingested message");
                encode(&set_read_action(vec![id.to_string()], !read))
            }),
            add_label => ("mail.message", "add-label", arg: "work", |_, res| {
                encode(&add_label_action(vec![message_id(res).to_string()], "work"))
            }),
            rename_folder => ("mail.folder:receipts", "rename", arg: "Invoices", |_, _| {
                encode(&rename_folder_action("receipts", "Invoices"))
            }),
            delete_folder => ("mail.folder:receipts", "delete-folder", |_, _| {
                encode(&delete_folder_action("receipts"))
            }),
            discard_draft => ("mail.folder:drafts", "discard-draft", |_, _| {
                encode(&discard_draft_action())
            }),
        ],
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn batch_flag_and_purge_actions_fail_closed_and_converge() {
        // Phase `mailbox_batch_actions_bind_stable_ids_and_purge_is_confirmed`.
        {
            let (app, state) = seeded_app();
            let resources = ui::resources::resources(&app, &state.doc);
            let id = app.mail_resource_ctx(&state.doc).messages[0].id.clone();
            let address = tilo_actions::ActionAddress::new("mail.mailbox", "archive-messages");
            let args = serde_json::json!({"ids": [id.clone(), id]});
            let encoded = tilo_actions::door::encode_json(
                &resources,
                &address,
                args.as_object().expect("object arguments"),
            )
            .expect("typed ids bind through the external resource door");
            assert_eq!(
                serde_json::from_slice::<MailAction>(&encoded.payload)
                    .expect("mail action decodes"),
                move_action(vec![id], Folder::Archive),
                "duplicate ids collapse before the canonical batch action is authored"
            );

            let purge = tilo_actions::door::survey(&resources)
                .into_iter()
                .find(|descriptor| descriptor.address.action == "purge-trashed-messages")
                .expect("published purge descriptor");
            assert_eq!(purge.params.len(), 1);
            assert_eq!(purge.params[0].name, "ids");
            assert_eq!(purge.params[0].ty, ParamType::Array);
            assert!(matches!(
                purge.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            ));
            assert!(
                !purge.contract.reversible,
                "a server-side permanent deletion must not claim local undo can restore it"
            );
        }

        // Phase `purge_action_refuses_mixed_or_stale_ids_without_deleting_a_valid_subset`.
        {
            let mut doc = AutoCommit::new();
            MailDoc::register_message(&mut doc, "inbox");
            MailDoc::register_message(&mut doc, "trash");
            MailDoc::set_folder_for(&mut doc, "inbox", Folder::Inbox);
            MailDoc::set_folder_for(&mut doc, "trash", Folder::Trash);
            let heads_before = doc.get_heads();
            let error = purge_action(vec!["inbox".into(), "missing".into(), "trash".into()])
                .apply(&mut doc)
                .expect_err("mixed/stale purge refuses rather than deleting a valid subset");
            assert!(matches!(error, ActionError::Refused(_)));
            assert!(MailDoc::is_present_message(&doc, "inbox"));
            assert!(MailDoc::is_present_message(&doc, "trash"));
            assert_eq!(MailDoc::get_folder_for(&doc, "inbox"), Some(Folder::Inbox));
            assert_eq!(doc.get_heads(), heads_before);
        }

        // Phase `purge_replays_to_peers_and_refuses_late_flag_resurrection`.
        {
            let mut a = AutoCommit::new();
            MailDoc::register_message(&mut a, "m");
            MailDoc::set_folder_for(&mut a, "m", Folder::Trash);
            a.commit();
            let mut b = AutoCommit::load(&a.save()).unwrap();

            let action = purge_action(vec!["m".into()]);
            let envelope = tilo_actions::apply_local(&mut a, &action, encode(&action)).unwrap();
            tilo_actions::replay(&mut b, &envelope, &registry()).unwrap();
            assert_eq!(a.get_heads(), b.get_heads(), "purge is replay-identical");
            assert!(!MailDoc::is_present_message(&b, "m"));

            let late = set_read_action(vec!["m".into()], true);
            late.apply(&mut b).unwrap();
            assert_eq!(MailDoc::get_read(&b, "m"), None);
        }

        // Phase `stale_or_unknown_message_actions_fail_closed_without_orphan_flags`.
        {
            let mut doc = AutoCommit::new();
            let action = set_read_action(vec!["not-a-loaded-message".into()], true);
            action.apply(&mut doc).unwrap();
            assert_eq!(
                MailDoc::get_read(&doc, "not-a-loaded-message"),
                None,
                "an action from a stale palette row cannot mint flags"
            );

            MailDoc::register_message(&mut doc, "gone");
            MailDoc::mark_message_purged(&mut doc, "gone");
            set_starred_action(vec!["gone".into()], true)
                .apply(&mut doc)
                .unwrap();
            assert_eq!(
                MailDoc::get_starred(&doc, "gone"),
                None,
                "an explicit tombstone refuses delayed replay"
            );
        }

        // Phase `replicated_text_and_batch_deltas_are_unicode_safe_and_bounded`.
        {
            let mut doc = AutoCommit::new();
            MailDoc::register_message(&mut doc, "m");
            let ids =
                std::iter::repeat_n("m".to_string(), MAX_MESSAGE_IDS_PER_ACTION + 20).collect();
            let label = format!("  {}\0ignored", "🦀".repeat(MAX_LABEL_CHARS + 4));
            MailAction::AddLabel { ids, label }.apply(&mut doc).unwrap();
            let labels = MailDoc::get_labels(&doc, "m");
            assert_eq!(labels.len(), 1, "duplicate ids are one logical target");
            assert_eq!(labels[0].chars().count(), MAX_LABEL_CHARS);
            assert!(labels[0].chars().all(|ch| !ch.is_control()));

            let draft = Draft {
                subject: "é".repeat(MAX_DRAFT_SUBJECT_CHARS + 10),
                body: format!("{}\0", "🦀".repeat(MAX_DRAFT_BODY_CHARS + 10)),
                ..Draft::default()
            };
            save_draft_action(&draft).apply(&mut doc).unwrap();
            let stored = MailDoc::load_draft(&doc);
            assert_eq!(stored.subject.chars().count(), MAX_DRAFT_SUBJECT_CHARS);
            assert_eq!(stored.body.chars().count(), MAX_DRAFT_BODY_CHARS);
            assert!(stored.body.is_char_boundary(stored.body.len()));
            assert!(!stored.body.contains('\0'));
        }

        // Phase `concurrent_same_message_flag_edits_merge_without_losing_either_field`.
        {
            let mut base = AutoCommit::new();
            MailDoc::register_message(&mut base, "m");
            base.commit();
            let bytes = base.save();
            let mut read_replica = AutoCommit::load(&bytes).unwrap();
            let mut star_replica = AutoCommit::load(&bytes).unwrap();
            read_replica.set_actor(automerge::ActorId::from(vec![1]));
            star_replica.set_actor(automerge::ActorId::from(vec![2]));

            set_read_action(vec!["m".into()], true)
                .apply(&mut read_replica)
                .unwrap();
            read_replica.commit();
            set_starred_action(vec!["m".into()], true)
                .apply(&mut star_replica)
                .unwrap();
            star_replica.commit();

            read_replica.merge_incoming(&mut star_replica).unwrap();
            assert_eq!(MailDoc::get_read(&read_replica, "m"), Some(true));
            assert_eq!(MailDoc::get_starred(&read_replica, "m"), Some(true));
        }
    }

    #[test]
    fn the_draft_body_is_rich_text_started_and_edited_by_actions() {
        use tilo_document::action::RegionKind;
        let text = |doc: &mut AutoCommit| {
            let body = MailDoc::draft_body(doc).expect("a body");
            tilo_rich_text::blocks_under(doc, &body)
                .unwrap()
                .into_iter()
                .map(|block| (block.kind, block.text))
                .collect::<Vec<_>>()
        };
        let mut doc = AutoCommit::new();
        assert!(MailDoc::draft_body(&doc).is_none());
        let refused =
            MailAction::EditDraftBody(RichTextAction::AppendParagraph { text: "x".into() });
        assert!(refused.apply(&mut doc).is_err(), "no body to edit yet");

        // A reply quote: a paragraph per line, `> ` lines quoted.
        MailAction::StartDraftBody(DraftBodySeed::text("Thanks!\n> original"))
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            text(&mut doc),
            [
                (ui::widget::BlockKind::Paragraph, "Thanks!".to_owned()),
                (ui::widget::BlockKind::Quote, "original".to_owned()),
            ]
        );
        let body = MailDoc::draft_body(&doc).unwrap();
        let first = tilo_rich_text::blocks_under(&doc, &body).unwrap()[0]
            .id
            .clone();
        MailAction::EditDraftBody(RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: first,
            at: 6,
            del: 1,
            ins: ", Ada.".into(),
        })
        .apply(&mut doc)
        .unwrap();
        assert_eq!(text(&mut doc)[0].1, "Thanks, Ada.");

        // A reopened draft starts from its HTML part.
        MailAction::StartDraftBody(DraftBodySeed {
            text: "Plan".into(),
            html: Some("<h2>Plan</h2><p><b>ship</b> it</p>".into()),
        })
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            text(&mut doc),
            [
                (ui::widget::BlockKind::Heading(2), "Plan".to_owned()),
                (ui::widget::BlockKind::Paragraph, "ship it".to_owned()),
            ]
        );
        MailAction::DiscardDraft.apply(&mut doc).unwrap();
        assert!(
            MailDoc::draft_body(&doc).is_none(),
            "discard clears the body"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn draft_folder_and_affordance_actions_apply_the_palette_action() {
        // Phase `save_then_discard_draft_action`.
        // Saving a draft persists it; discarding clears it — both through the action.
        {
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

        // Phase `new_folder_action_creates_a_local_folder`.
        // The "New folder" prompted action mints an id and creates a folder in the
        // registry.
        {
            let (app, state) = seeded_app();
            let resources = ui::resources::resources(&app, &state.doc);
            let spec = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "new-folder")
                .unwrap();
            let encoded = prompted_encode(spec, "Receipts");
            let mut doc = state.doc;
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
            let folders = MailDoc::list_folders(&doc);
            assert_eq!(folders.len(), 1, "one local folder created");
            assert_eq!(folders[0].1, "Receipts");
        }

        // Phase `affordance_dispatches_the_palette_action`.
        // Single source of truth #2: the on-screen message affordance
        // ([`MailState::apply_message_action`]) dispatches the byte-identical action
        // the palette's archive spec builds — proven on the real dispatch path via
        // the framework [`ui::ActionHost`].
        {
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
}
