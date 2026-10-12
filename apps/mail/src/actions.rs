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

use serde::{Deserialize, Serialize};
use tilo_document::RichTextAction;
use ui::tilo_resource::{self, Effect};

use tilo_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ParamType, Resource,
};

use crate::attachment_store;
use crate::doc::MailDoc;
use crate::model::{Draft, DraftAttachment, Folder};

/// Registry `type_id` for every Mail action.
pub const MAIL_ACTION_TYPE: &str = "mail.action";

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

/// What a Mail view effect opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailTarget {
    /// A system folder, by key.
    Folder(String),
    /// An account, by its id in this device's account store, so the address
    /// keeps naming the same account when another is added or removed.
    Account(String),
    /// A loaded message: opening it is the read boundary, so it records the
    /// message read through the same `SetRead` action as every control.
    Message(String),
    /// The saved draft, back in the composer.
    Draft,
}

/// What Mail does that changes no document: what the window shows, the
/// composer it opens, fetching from the server, saving an attachment on this
/// device, and preparing an invitation as the draft. Effects on one item
/// ride that item; the ones a caller gives arguments to are offered on the
/// mailbox.
#[derive(Debug, Clone, PartialEq, Effect)]
#[effect(operation = "mail.effects")]
pub enum MailEffect {
    /// Compose message
    ///
    /// Opens a new local composition; until it is saved as a draft or sent
    /// it is this window's own.
    #[effect(view)]
    Compose,
    /// Choose a recipient and sending account from shared Contacts.
    #[effect(view)]
    ContactRecipients,
    /// Prepare a draft from this exact address-book recipient and sender.
    ///
    /// Requires review; creates no identity link and sends nothing.
    #[effect(row, destructive, returns)]
    PrepareContactDraft {
        person: String,
        label: String,
        email: String,
        sender_account: String,
        sender_email: String,
    },
    /// Refresh current folder
    #[effect(job, idempotent)]
    Refresh,
    /// Prepare calendar invitation
    ///
    /// Saves a bounded METHOD:REQUEST invitation as the draft and opens it
    /// for review; nothing is sent.
    #[effect(permission = "invite", idempotent)]
    PrepareCalendarInvitation {
        #[param("Invitation recipient")]
        to: String,
        #[param("Invitation subject")]
        subject: String,
        #[param("Plain-text invitation message")]
        body: String,
        #[param("Bounded text/calendar METHOD:REQUEST payload")]
        calendar_ics: String,
    },
    /// Open
    #[effect(view, row)]
    Open { target: MailTarget },
    /// Preview without marking read
    ///
    /// Shows a loaded message without changing its read state: asking to
    /// inspect a message never sends a read receipt.
    #[effect(view, row)]
    Inspect { message: String },
    /// Reply
    #[effect(view, row)]
    Reply { message: String },
    /// Reply all
    #[effect(view, row)]
    ReplyAll { message: String },
    /// Forward
    #[effect(view, row)]
    Forward { message: String },
    /// Save attachment
    ///
    /// Saves a downloaded attachment where the person chooses on this
    /// device.
    #[effect(row, job, cancellable)]
    Save { message: String, index: usize },
}

/// What invoking `effect` where it is bound sends its handler.
#[cfg(test)]
pub(crate) fn effect_payload(effect: &MailEffect) -> Vec<u8> {
    effect
        .bound()
        .build_json(&serde_json::Map::new())
        .map(|encoded| encoded.payload)
        .unwrap_or_default()
}

/// `effect` bound to the item it acts on, under that item's own `label`.
fn bound(effect: &MailEffect, label: &str) -> ActionSpec {
    let mut spec = effect.bound();
    label.clone_into(&mut spec.label);
    spec
}

/// The persisted scalar fields of the in-progress draft, in exactly the shape
/// [`MailDoc::store_draft`] writes. Carrying the document shape keeps
/// [`Action::apply`] a pure write with no re-derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DraftData {
    #[serde(default)]
    pub sender: Option<crate::model::DraftSender>,
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
            sender: d.sender.clone(),
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
            sender: self.sender.clone(),
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
    /// The mail server confirmed it holds what the update asked of it.
    ServerApplied(ServerUpdate),
    /// Send a message: it waits in the mailbox's outbox until a device that
    /// holds its account delivers it. Taking this back before then takes the
    /// message out of the outbox unsent.
    Send(Outgoing),
    /// Take a message out of the outbox unsent: one still waiting, or one
    /// its server refused.
    CancelSend { id: String },
    /// The account's server accepted the message: it leaves the outbox.
    Delivered { id: String },
    /// The account's server refused the message; it stays in the outbox
    /// with the reason, and is not tried again.
    SendFailed { id: String, reason: String },
}

/// A message waiting in the outbox: the account it is sent from (by its
/// address; the device that delivers it holds the account), what it says,
/// and the message it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outgoing {
    pub id: String,
    pub account: String,
    pub draft: DraftData,
    #[serde(default)]
    pub in_reply_to: Option<String>,
}

impl Outgoing {
    /// `draft`, sent from `account`, under a new id.
    pub fn new(account: &str, draft: &Draft) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            account: bounded_text(account, MAX_DRAFT_ADDRESS_CHARS, false),
            draft: DraftData::from_draft(draft),
            in_reply_to: draft.in_reply_to.map(|id| id.to_string()),
        }
    }

    /// The draft the engine sends.
    pub fn to_draft(&self) -> Draft {
        Draft {
            in_reply_to: self.in_reply_to.as_deref().and_then(|id| id.parse().ok()),
            ..self.draft.to_draft()
        }
    }

    fn is_valid(&self) -> bool {
        valid_outbox_id(&self.id) && self.account.contains('@') && !self.draft.to.trim().is_empty()
    }
}

fn valid_outbox_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_MESSAGE_ID_CHARS
}

/// A message id plus the flags its source reports, used where the document
/// has none yet. A message fetched from a server also names where it lives
/// there: the account and its `Message-ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestBaseline {
    pub id: String,
    pub read: Option<bool>,
    pub starred: Option<bool>,
    pub folder: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub locator: Option<String>,
}

impl IngestBaseline {
    /// A message that exists only here: a draft, a sent copy, the sample.
    pub fn from_message(message: &crate::model::Message) -> Self {
        Self {
            id: message.id.to_string(),
            read: Some(message.read),
            starred: Some(message.starred),
            folder: Some(message.folder.as_key().to_string()),
            account: None,
            locator: None,
        }
    }

    /// A message `account`'s server reported, as it reported it. One without
    /// a `Message-ID` cannot be found on the server again, so it is recorded
    /// like a local one.
    pub fn fetched(message: &crate::model::Message, account: &str) -> Self {
        let locator = message
            .rfc_message_id
            .as_deref()
            .map(str::trim)
            .filter(|locator| !locator.is_empty() && !account.trim().is_empty());
        Self {
            account: locator.map(|_| account.to_owned()),
            locator: locator.map(str::to_owned),
            ..Self::from_message(message)
        }
    }

    pub fn register_only(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            read: None,
            starred: None,
            folder: None,
            account: None,
            locator: None,
        }
    }
}

/// One change a mail server still has to be told about, as
/// [`MailDoc::server_updates`] finds it: the message, where it lives on the
/// server, and what to do there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerUpdate {
    /// Set the message's read and starred flags (`UID STORE`).
    Flags {
        id: String,
        account: String,
        locator: String,
        folder: Folder,
        read: bool,
        starred: bool,
    },
    /// Move the message from one server folder to another (`UID MOVE`).
    Move {
        id: String,
        account: String,
        locator: String,
        from: Folder,
        to: Folder,
    },
    /// Delete the message for good (`UID STORE \Deleted` + `UID EXPUNGE`).
    Remove {
        id: String,
        account: String,
        locator: String,
        folder: Folder,
    },
}

impl ServerUpdate {
    /// The message it is about.
    pub fn id(&self) -> &str {
        match self {
            Self::Flags { id, .. } | Self::Move { id, .. } | Self::Remove { id, .. } => id,
        }
    }

    /// The account whose server it goes to.
    pub fn account(&self) -> &str {
        match self {
            Self::Flags { account, .. }
            | Self::Move { account, .. }
            | Self::Remove { account, .. } => account,
        }
    }

    /// The message as the engine finds it on the server: by its
    /// `Message-ID`, in the folder it is in there, with the flags to set.
    pub fn locate(&self) -> crate::model::Message {
        let (locator, folder, read, starred) = match self {
            Self::Flags {
                locator,
                folder,
                read,
                starred,
                ..
            } => (locator, *folder, *read, *starred),
            Self::Move { locator, from, .. } => (locator, *from, false, false),
            Self::Remove {
                locator, folder, ..
            } => (locator, *folder, false, false),
        };
        crate::model::Message {
            id: self.id().parse().unwrap_or_default(),
            rfc_message_id: Some(locator.clone()),
            in_reply_to: None,
            from: crate::model::Addr::new("", ""),
            to: Vec::new(),
            cc: Vec::new(),
            subject: String::new(),
            body: String::new(),
            html_body: None,
            attachments: Vec::new(),
            timestamp_min: 0,
            read,
            starred,
            folder,
            labels: Vec::new(),
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
                    MailDoc::ingest_baseline(doc, message);
                }
            }
            MailAction::ServerApplied(update) => MailDoc::server_applied(doc, update),
            MailAction::Send(outgoing) => {
                if !outgoing.is_valid() {
                    return Err(ActionError::Refused(
                        "a message needs a sending account and a recipient".into(),
                    ));
                }
                let bounded = Outgoing {
                    draft: DraftData::from_draft(&outgoing.to_draft()),
                    ..outgoing.clone()
                };
                MailDoc::queue_outgoing(doc, &bounded);
            }
            MailAction::CancelSend { id } | MailAction::Delivered { id } => {
                if valid_outbox_id(id) {
                    MailDoc::remove_outgoing(doc, id);
                }
            }
            MailAction::SendFailed { id, reason } => {
                if valid_outbox_id(id) {
                    MailDoc::fail_outgoing(
                        doc,
                        id,
                        &bounded_text(reason, MAX_FOLDER_NAME_CHARS * 4, false),
                    );
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// Whether a message was read is state about it, not a change to it; what
    /// a mail server reports is content from outside, which no undo takes back.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            MailAction::SetRead { .. } => tilo_actions::ChangeKind::Housekeeping,
            MailAction::IngestBaselines { .. }
            | MailAction::ServerApplied(_)
            | MailAction::Delivered { .. }
            | MailAction::SendFailed { .. } => tilo_actions::ChangeKind::External,
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

// ===========================================================================
// Resource tree
// ===========================================================================

/// Largest number of message rows materialised into the published resource
/// tree. Individual messages and threads remain lazily resolvable by
/// stable id, so this is a snapshot-memory ceiling rather than a discovery cap.
pub(crate) const OPEN_MESSAGE_LIMIT: usize = 100;

/// One message surfaced as a resource, with the scalar state its actions need
/// resolved at authoring time (so "Mark read" builds the byte-identical action
/// the on-screen control does).
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub subject: String,
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
/// its canonical resource, so a lazily resolved message, thread or attachment
/// row is built without cloning the entire mailbox.
pub(crate) fn message_row(message: &crate::model::Message) -> MessageRow {
    MessageRow {
        id: message.id.to_string(),
        subject: message.subject.clone(),
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

fn message_label(m: &MessageRow) -> String {
    let s = m.subject.trim();
    let base = if s.is_empty() { "(no subject)" } else { s };
    if m.starred {
        format!("{base} (starred)")
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
    resource = with_message_views(resource, &message.id);
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
    let save = MailEffect::Save {
        message: message_id.to_owned(),
        index,
    }
    .bound();
    Resource::new(
        format!("mail.attachment:{message_id}:{index}"),
        "mail.attachment",
        format!(
            "{} ({})",
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

/// Open, preview, reply, reply all and forward, bound to the message
/// `id`: what can be done with a message that changes no document.
fn with_message_views(resource: Resource, id: &str) -> Resource {
    let message = || id.to_owned();
    resource
        .with_action(bound(
            &MailEffect::Open {
                target: MailTarget::Message(message()),
            },
            "Open message",
        ))
        .with_action(MailEffect::Inspect { message: message() }.bound())
        .with_action(MailEffect::Reply { message: message() }.bound())
        .with_action(MailEffect::ReplyAll { message: message() }.bound())
        .with_action(MailEffect::Forward { message: message() }.bound())
}

fn open_folder(folder: Folder) -> ActionSpec {
    bound(
        &MailEffect::Open {
            target: MailTarget::Folder(folder.as_key().to_owned()),
        },
        "Open folder",
    )
}

fn open_draft() -> ActionSpec {
    bound(
        &MailEffect::Open {
            target: MailTarget::Draft,
        },
        "Continue editing",
    )
}

/// This device's accounts `(id, label, active)`, each opening its inbox.
pub(crate) fn accounts_collection(accounts: &[(String, String, bool)]) -> Resource {
    Resource::new("mail.accounts", "mail.account.collection", "Accounts").with_children(
        accounts.iter().map(|(id, label, active)| {
            Resource::new(
                format!("mail.account:{id}"),
                "mail.account",
                if *active {
                    format!("{label} (open)")
                } else {
                    label.clone()
                },
            )
            .with_action(bound(
                &MailEffect::Open {
                    target: MailTarget::Account(id.clone()),
                },
                "Open account",
            ))
        }),
    )
}

/// Mailbox-level Document ops for [`tilo_resource::publish`] — bulk triage and
/// new-folder. Compose, refresh, and the invitation are [`MailEffect`]s.
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
/// delete / labels — and the `open` view a search hit on the message takes.
/// Reply / forward / save-attachment stay Views/Effects of the window.
pub(crate) fn publish_message_document(message: &MessageRow) -> Resource {
    let mut resource = with_message_views(
        Resource::new(
            format!("mail.message:{}", message.id),
            "mail.message",
            message_label(message),
        ),
        &message.id,
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
    )
    .with_action(open_folder(folder));
    if folder == Folder::Drafts && has_draft {
        folder_node = folder_node.with_action(
            immediate_spec("discard-draft", "Discard draft", &discard_draft_action()).destructive(),
        );
        folder_node = folder_node.with_child(
            Resource::new("mail.draft:current", "mail.draft", "Current draft")
                .with_action(open_draft())
                .with_action(
                    immediate_spec("discard", "Discard draft", &discard_draft_action())
                        .destructive(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MailApp, avatar_store_offline};
    use crate::model::Folder;
    use crate::resource::MailShell;
    use crate::state::{CtxAction, MailState};
    use automerge::AutoCommit;
    use tilo_automerge::ApplyIncoming;

    use tilo_actions::LocalTransport;
    use ui::framework::testing::TestApp;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
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

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does.
    fn dispatch(doc: &mut AutoCommit, action: &MailAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// The sample mailbox window as the framework hosts it.
    fn sample_window() -> TestApp<MailShell> {
        ui::i18n::I18nManager::init("en-US");
        TestApp::<MailShell>::capture(Some("wide")).expect("the sample mailbox captures")
    }

    /// Everything the window publishes, as the host's palette and agents see it.
    fn published(app: &mut TestApp<MailShell>) -> Vec<Resource> {
        app.harness().resources()
    }

    /// The published mailbox root.
    fn mailbox(resources: &[Resource]) -> &Resource {
        let mut roots = resources.iter().filter(|r| r.kind == "mail.mailbox");
        let root = roots.next().expect("a published mailbox");
        assert!(roots.next().is_none(), "one mailbox root");
        root
    }

    /// The published resource `id`, wherever it sits in the tree.
    fn find<'a>(resources: &'a [Resource], id: &str) -> Option<&'a Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    /// Resolve `id` as the host resolves an address the tree does not list.
    fn resolve(app: &mut TestApp<MailShell>, id: &str) -> Option<Resource> {
        let harness = app.harness();
        ui::resources::resource(harness.root_widget(), &harness.state().doc, id)
    }

    /// The ids of the document's messages in the Inbox.
    fn inbox_ids(doc: &AutoCommit) -> Vec<String> {
        MailDoc::list_message_flags(doc)
            .into_iter()
            .filter(|flags| flags.folder == Folder::Inbox)
            .map(|flags| flags.id)
            .collect()
    }

    /// The mailbox resource tree: open vs neutral preview, bounded snapshots, stable
    /// thread/account addresses, and one purge action for trash.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn mailbox_resources_publish_bounded_stable_addresses() {
        // Phase `message_resource_distinguishes_open_from_read_state_neutral_preview`.
        {
            let mut app = sample_window();
            let resources = published(&mut app);
            let unread = app
                .state()
                .messages
                .into_iter()
                .find(|message| message.folder == Folder::Inbox && !message.read)
                .expect("sample has an unread inbox message");
            let resource = find(&resources, &format!("mail.message:{}", unread.id))
                .expect("the unread message is published");
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
            let payload = |spec: &ActionSpec| {
                spec.build_json(&serde_json::Map::new())
                    .expect("a bound view takes no arguments")
                    .payload
            };
            assert!(matches!(
                MailEffect::from_payload(&payload(open)).unwrap(),
                MailEffect::Open {
                    target: MailTarget::Message(_)
                }
            ));
            assert!(matches!(
                MailEffect::from_payload(&payload(preview)).unwrap(),
                MailEffect::Inspect { .. }
            ));
        }

        // Phase `ctrl_k_publishes_the_mailbox_tree`.
        // Ctrl+K on Mail yields the mailbox root exporting "New folder", the
        // folders with `mail.message:<id>` children exporting read / star /
        // archive / delete, and surfaces them in the one palette.
        {
            // The window's effects ride the mailbox where someone uses it; a
            // capture runs none, so they are read from a live window.
            let mut live = TestApp::<MailShell>::new().unwrap();
            let live_resources = published(&mut live);
            let live_mailbox = mailbox(&live_resources);
            let effects: Vec<&str> = live_mailbox.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(effects.contains(&"compose"), "{effects:?}");
            assert!(effects.contains(&"refresh"), "{effects:?}");
            ui::testing::action_contract::assert_ctrl_k_publishes(
                &live_resources,
                &["Compose message", "New folder"],
            );
            let refresh = live_mailbox
                .actions
                .iter()
                .find(|a| a.id == "refresh")
                .unwrap();
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

            let mut app = sample_window();
            let resources = published(&mut app);
            let violations = tilo_actions::validate_surface_contracts(&resources, &registry());
            assert!(violations.is_empty(), "{violations:?}");
            let mailbox = mailbox(&resources);
            let mb_actions: Vec<&str> = mailbox.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(mb_actions.contains(&"new-folder"), "{mb_actions:?}");
            for id in [
                "archive-messages",
                "move-messages-to-trash",
                "mark-messages-read",
                "mark-messages-unread",
                "purge-trashed-messages",
            ] {
                assert!(mb_actions.contains(&id), "mailbox exposes typed batch {id}");
            }

            let inbox = mailbox
                .children
                .iter()
                .find(|r| r.id.as_str() == "mail.folder:inbox")
                .expect("the inbox folder node");
            assert!(
                !inbox.children.is_empty() && inbox.children.len() <= OPEN_MESSAGE_LIMIT,
                "the inbox publishes a bounded page of its messages"
            );
            assert!(
                inbox
                    .children
                    .iter()
                    .all(|item| item.kind == "mail.message"),
                "an inbox child is a message"
            );

            // A loaded thread stays addressable though the tree does not list it.
            let thread = app
                .with_surface::<MailApp, _>(|mail| {
                    mail.shared_mailbox().threads_in(Folder::Inbox)[0].clone()
                })
                .expect("the window is Mail");
            let resolved = resolve(&mut app, &format!("mail.thread:inbox:{}", thread.id))
                .expect("a loaded thread resolves by its stable address");
            assert_eq!(resolved.kind, "mail.thread");
            assert_eq!(resolved.children.len(), thread.message_ids.len());
            assert!(
                resolved
                    .children
                    .iter()
                    .all(|item| item.kind == "mail.message")
            );

            let id = inbox_ids(&app.read_document(Clone::clone))[0].clone();
            let message = resolve(&mut app, &format!("mail.message:{id}"))
                .expect("a message remains addressable by its id");
            let msg_actions: Vec<&str> = message.actions.iter().map(|a| a.id.as_str()).collect();
            for want in ["read", "star", "archive", "delete", "add-label", "open"] {
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

        // Phase `a_large_mailbox_does_not_expand_the_published_tree`.
        {
            let mut app = TestApp::<MailShell>::new().unwrap();
            let count = OPEN_MESSAGE_LIMIT * 5;
            app.merge_peer_edit(|doc| {
                for id in 0..count {
                    let id = id.to_string();
                    MailDoc::register_message(doc, &id);
                    MailDoc::set_folder_for(doc, &id, Folder::Inbox);
                }
            });
            let resources = published(&mut app);
            let inbox = find(&resources, "mail.folder:inbox").expect("inbox resource");
            assert_eq!(
                inbox.children.len(),
                OPEN_MESSAGE_LIMIT,
                "the published page is bounded"
            );
            let last = (count - 1).to_string();
            assert!(find(&resources, &format!("mail.message:{last}")).is_none());
            let far = resolve(&mut app, &format!("mail.message:{last}"))
                .expect("a message past the page still resolves");
            assert!(far.actions.len() >= 5);
        }

        // Phase `threads_keep_distinct_stable_resource_addresses`.
        // A thread's address is its conversation anchor, never its subject:
        // every loaded thread resolves to exactly its own messages.
        {
            let mut app = sample_window();
            let threads = app
                .with_surface::<MailApp, _>(|mail| mail.shared_mailbox().threads_in(Folder::Inbox))
                .expect("the window is Mail");
            assert!(threads.len() > 1, "the sample inbox has several threads");
            let mut seen = std::collections::HashSet::new();
            for thread in threads {
                let id = format!("mail.thread:inbox:{}", thread.id);
                assert!(seen.insert(id.clone()), "{id} names one thread");
                let resolved = resolve(&mut app, &id).expect("the thread resolves");
                let messages: Vec<String> = resolved
                    .children
                    .iter()
                    .map(|message| message.id.as_str().to_owned())
                    .collect();
                let expected: Vec<String> = thread
                    .message_ids
                    .iter()
                    .map(|message| format!("mail.message:{message}"))
                    .collect();
                assert_eq!(messages, expected, "{id}");
            }
        }

        // Phase `account_resource_uses_a_durable_id_not_its_display_index`.
        {
            let mut app = sample_window();
            let resources = published(&mut app);
            let accounts = find(&resources, "mail.accounts").expect("the accounts row");
            assert_eq!(accounts.children.len(), 2, "both sample accounts");
            let other = accounts
                .children
                .iter()
                .find(|account| !account.label.ends_with("· Active"))
                .expect("an inactive account");
            let raw = other
                .id
                .as_str()
                .strip_prefix("mail.account:")
                .expect("an account address")
                .to_owned();
            assert!(
                raw.parse::<usize>().is_err(),
                "an account is addressed by its id, not its position: {raw}"
            );
            let address = tilo_actions::ActionAddress::new(other.id.as_str(), "open");
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .expect("the stable account address resolves through the mailbox tree");
            assert_eq!(
                MailEffect::from_payload(&encoded.payload).unwrap(),
                MailEffect::Open {
                    target: MailTarget::Account(raw)
                }
            );
        }

        // Phase `trash_resource_uses_the_same_purge_action_as_the_delete_affordance`.
        {
            let row = MessageRow {
                id: "m".into(),
                subject: "Already trashed".into(),
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

    /// The window a contract seed was staged in, and the message the seed
    /// moved to Trash: the resources are what that very window publishes.
    struct Seeded {
        app: TestApp<MailShell>,
        trashed: String,
    }

    thread_local! {
        static SEEDED: std::cell::RefCell<Option<Seeded>> = const { std::cell::RefCell::new(None) };
    }

    fn with_seeded<R>(f: impl FnOnce(&mut Seeded) -> R) -> R {
        SEEDED.with_borrow_mut(|seeded| f(seeded.as_mut().expect("the contract seed ran first")))
    }

    /// The sample mailbox after a user trashed one message, labelled another,
    /// created a local folder and started a draft — all through the actions.
    fn mailbox_seed() -> AutoCommit {
        use ui::tilo_resource::Invoke;
        let mut app = sample_window();
        let ids = inbox_ids(&app.read_document(Clone::clone));
        let trashed = ids.last().cloned().expect("sample inbox has messages");
        for action in [
            move_action(vec![trashed.clone()], Folder::Trash),
            add_label_action(vec![ids[0].clone()], "work"),
            create_folder_action("receipts", "Receipts"),
            save_draft_action(&Draft {
                to: "ada@example.test".into(),
                subject: "Plans".into(),
                ..Default::default()
            }),
        ] {
            app.invoke_confirmed(Invoke::apply(&action).unwrap())
                .unwrap();
        }
        app.settle();
        let doc = app.read_document(Clone::clone);
        SEEDED.set(Some(Seeded { app, trashed }));
        doc
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
        resources: |_| with_seeded(|seeded| published(&mut seeded.app)),
        palette: ["New folder", "Archive", "Rename folder", "Discard draft"],
        replay: [
            set_starred => |doc| {
                let id = inbox_ids(doc)
                    .into_iter()
                    .find(|id| MailDoc::get_starred(doc, id) == Some(false))
                    .expect("an unstarred inbox message");
                encode(&set_starred_action(vec![id], true))
            },
            batched_move => |doc| encode(&move_action(
                inbox_ids(doc).into_iter().take(3).collect(),
                Folder::Archive,
            )),
            add_label => |doc| encode(&add_label_action(vec![inbox_ids(doc)[1].clone()], "urgent")),
            purge => |_| encode(&purge_action(vec![with_seeded(|seeded| seeded.trashed.clone())])),
        ],
        undo: [
            move_to_trash => |doc| encode(&move_action(vec![inbox_ids(doc)[0].clone()], Folder::Trash)),
        ],
        encodes: [
            archive => ("mail.message", "archive", |_, res| {
                encode(&move_action(vec![message_id(res).to_string()], Folder::Archive))
            }),
            read => ("mail.message", "read", |doc, res| {
                let id = message_id(res);
                let read = MailDoc::get_read(doc, id).unwrap_or(false);
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
            let mut app = sample_window();
            let resources = published(&mut app);
            let id = inbox_ids(&app.read_document(Clone::clone))[0].clone();
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
            let mut app = sample_window();
            let resources = published(&mut app);
            let spec = mailbox(&resources)
                .actions
                .iter()
                .find(|a| a.id == "new-folder")
                .unwrap();
            let encoded = spec.build_text("Receipts").unwrap();
            let mut doc = app.read_document(Clone::clone);
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
            let folders = MailDoc::list_folders(&doc);
            assert_eq!(folders.len(), 1, "one local folder created");
            assert_eq!(folders[0].1, "Receipts");
        }

        // Phase `affordance_dispatches_the_palette_action`.
        // Single source of truth #2: the on-screen message affordance
        // ([`MailState::apply_message_action`]) writes what the palette's
        // archive action writes, through the framework dispatcher.
        {
            let mut st = MailState::new_sample(avatar_store_offline());
            let mut doc = AutoCommit::new();
            let id = an_inbox_id(&st);

            st.apply_message_action(
                &mut ui::widget::Edits::lend(&mut doc),
                id.parse().unwrap(),
                CtxAction::Archive,
            );
            let mut palette = AutoCommit::new();
            MailDoc::register_message(&mut palette, &id);
            move_action(vec![id.clone()], Folder::Archive)
                .apply(&mut palette)
                .unwrap();
            assert_eq!(
                MailDoc::list_message_flags(&doc),
                MailDoc::list_message_flags(&palette),
                "the archive affordance and the palette make the identical edit"
            );
            // And it really moved the message in the document.
            assert_eq!(MailDoc::get_folder_for(&doc, &id), Some(Folder::Archive));
        }
    }
}
