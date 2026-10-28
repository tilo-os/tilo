//! What Mail publishes beside its mailbox, and the values its verbs take.
//!
//! The mailbox record and its verbs are [`crate::resource::Mail`]. Here are
//! Mail's effects (what changes no document: what the window shows, the
//! composer, fetching, saving an attachment, preparing an invitation), the
//! values the verbs carry (a message to send, what a fetch reports, what a
//! server confirmed), and the rows the window shows of mail the engine
//! loaded: a message, its attachments, the accounts.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Effect, JsonSchema};

use tilo_actions::{ActionSpec, Resource};

use crate::attachment_store;
use crate::model::{Draft, DraftAttachment, Folder};
use crate::resource::{MAX_DRAFT_ADDRESS_CHARS, MAX_DRAFT_BODY_CHARS, MAX_DRAFT_SUBJECT_CHARS};

const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_CALENDAR_ICS_BYTES: usize = 256 * 1024;

/// Take at most `max` Unicode scalar values without slicing a UTF-8 byte
/// offset.  Control characters are removed from one-line fields; bodies keep
/// normal newlines but still reject NUL and other control injection.
pub(crate) fn bounded_text(value: &str, max: usize, multiline: bool) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control() || (multiline && matches!(ch, '\n' | '\r' | '\t')))
        .take(max)
        .collect()
}

/// Normalise a batch before applying it.  This both bounds work and makes a
/// duplicated id in a hostile payload behave exactly like one requested id.
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
    #[effect(idempotent)]
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
    /// Read message
    ///
    /// Answers with what a loaded message says: who sent it to whom, its
    /// subject, when, its text and its attachments' names. Changes nothing,
    /// and marks it read for no one.
    #[effect(row, kind = "read", returns, idempotent)]
    ReadMessage { message: String },
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
    /// Load remote images
    ///
    /// Shows the images, fonts and styles an HTML message loads from the
    /// web, for this message only. Loading them tells the sender it was
    /// read; nothing else it asks for (script, frames, forms) ever runs.
    #[effect(view, row)]
    LoadRemoteContent { message: String },
    /// Always load remote images from this sender
    ///
    /// Loads them in every message from this message's sender, now and
    /// later. Part of the person's Mail preferences.
    #[effect(view, row)]
    AlwaysLoadRemoteContent { message: String },
    /// Search messages
    ///
    /// Finds the messages held in the mailbox whose sender, recipients,
    /// subject or text hold every word, newest first. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    SearchMessages {
        #[param("The words to find, ignoring case")]
        query: String,
        #[param("The folder to search; every folder when omitted")]
        folder: Option<crate::model::Folder>,
    },
    /// Export content
    ///
    /// Keeps held mail as a file in the room's content and answers its
    /// content address: one message as Markdown, JSON or .eml, or a
    /// folder's or every message as Markdown or JSON. No file is written
    /// and nothing is sent.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportContent {
        #[param("md, json or eml; md when omitted", choices = ["md", "json", "eml"])]
        format: Option<String>,
        #[param("The id of one held message; every message when omitted")]
        message: Option<String>,
        #[param("The folder whose messages to export")]
        folder: Option<crate::model::Folder>,
    },
    /// Open link
    ///
    /// Opens one of the message's links (as Read message lists them) in the
    /// Browser.
    #[effect(view, row)]
    OpenLink {
        message: String,
        #[param("A link of the message, as Read message lists it")]
        url: String,
    },
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
    label.clone_into(&mut spec.type_mut().label);
    spec
}

/// A message as written: who it goes to, what it says, what it carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
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

    pub(crate) fn to_draft(&self) -> Draft {
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
            wire: crate::model::WireIds::default(),
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

/// A message waiting in the outbox: the account it is sent from (by its
/// address; the device that delivers it holds the account), what it says,
/// and the message it answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outgoing {
    pub id: String,
    pub account: String,
    pub draft: DraftData,
    #[serde(default)]
    pub in_reply_to: Option<String>,
}

impl Outgoing {
    /// `draft`, sent from `account`, under a new id, as the mailbox's send
    /// verb queues it.
    #[cfg(test)]
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
            wire: crate::model::WireIds::default(),
            ..self.draft.to_draft()
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        valid_outbox_id(&self.id)
            && self.account.contains('@')
            && [&self.draft.to, &self.draft.cc, &self.draft.bcc]
                .into_iter()
                .any(|field| !crate::transport::parse_addr_list(field).is_empty())
    }
}

tilo_resource::json_model!(Outgoing);

fn valid_outbox_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_MESSAGE_ID_CHARS
}

/// A message id plus the flags its source reports, used where the document
/// has none yet. A message fetched from a server also names where it lives
/// there: the account and its `Message-ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
/// [`Mail::server_updates`](crate::resource::Mail::server_updates) finds
/// it and the mailbox queues it: the message, where it lives on the
/// server, and what to do there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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

tilo_resource::json_model!(ServerUpdate);

/// Nothing to tell: what a queue entry reads as before it is written.
impl Default for ServerUpdate {
    fn default() -> Self {
        Self::Flags {
            id: String::new(),
            account: String::new(),
            locator: String::new(),
            folder: Folder::Inbox,
            read: false,
            starred: false,
        }
    }
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
            account: String::new(),
            server: None,
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
}

/// One loaded message as the window shows it beside the mailbox.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub subject: String,
    pub starred: bool,
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
        starred: message.starred,
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
    let mut resource = with_message_views(
        Resource::new(
            format!("mail.message:{}", message.id),
            "mail.message",
            message_label(message),
        ),
        &message.id,
    );
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
            ui::format::size(attachment.size)
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
        .with_action(MailEffect::ReadMessage { message: message() }.bound())
        .with_action(MailEffect::Reply { message: message() }.bound())
        .with_action(MailEffect::ReplyAll { message: message() }.bound())
        .with_action(MailEffect::Forward { message: message() }.bound())
        .with_action(MailEffect::LoadRemoteContent { message: message() }.bound())
        .with_action(MailEffect::AlwaysLoadRemoteContent { message: message() }.bound())
        .with_action(
            MailEffect::OpenLink {
                message: message(),
                url: String::new(),
            }
            .bound_asking(&["url"]),
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

#[cfg(test)]
mod tests;
