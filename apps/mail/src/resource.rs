//! Mail's mailbox: what the reader's devices share of their mail, its
//! verbs, the places derived from it, and the app.
//!
//! The mail itself (headers, bodies, attachments) stays with the engine and
//! the server; the mailbox holds what the reader did with each message —
//! read, starred, which folder, labels, deleted for good — and what its
//! server holds of that, the folders the reader made, the message being
//! written, and the messages sent and not yet delivered. Starring, moving
//! one message, labels, folder names and the draft's fields are field edits;
//! the methods below are what is more than setting a field. Where a
//! message's flags and folder differ from what its server holds, the
//! mailbox queues telling the server ([`Mail::server_updates`] into
//! [`Mail::server`]), whoever made the change — a click, the palette, an
//! agent, a collaborator or an undo; the device with the account carries it
//! out and records what the server confirmed.

use tilo_document::{RichText, RichTextAction};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{
    self, ActionCx, Collection, Derived, Id, Model, Outbox, Refusal, SearchEntry, Set, Settled,
    resource,
};
use ui::widget::Element;

use crate::actions::{
    DraftData, IngestBaseline, MailEffect, MailTarget, Outgoing, ServerUpdate, bounded_text,
    valid_calendar_invitation,
};
use crate::app::MailApp;
use crate::model::{Draft, DraftAttachment, DraftSender, Folder};
use crate::sample::MailFixture;

/// The most messages one verb acts on at once.
pub(crate) const MAX_MESSAGES_PER_ACTION: usize = 256;
const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_LABEL_CHARS: usize = 128;
const MAX_FOLDER_NAME_CHARS: usize = 96;
pub(crate) const MAX_DRAFT_ADDRESS_CHARS: usize = 8 * 1024;
pub(crate) const MAX_DRAFT_SUBJECT_CHARS: usize = 998;
pub(crate) const MAX_DRAFT_BODY_CHARS: usize = 256 * 1024;

/// How many settled server updates and sent messages the mailbox keeps.
const SETTLED_KEPT: usize = 64;

/// How many messages the published mailbox carries as places; the rest are
/// the window's loaded rows.
pub(crate) const OPEN_MESSAGE_LIMIT: usize = 100;

/// What the reader's devices share of their mail.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Mail {
    /// What the mailbox knows of each message, under its id.
    pub messages: Collection<MessageState>,
    /// The folders the reader made, under their ids.
    pub folders: Collection<LocalFolder>,
    /// The message being written.
    pub draft: DraftState,
    /// Messages sent and waiting for a device that holds their account.
    pub outbox: Outbox<Outgoing>,
    /// What servers still have to be told: flags, moves and deletes the
    /// reader made that a message's server does not hold yet.
    pub server: Outbox<ServerUpdate>,
}

/// What the reader did with one message, and what its server holds.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = MessageState::valid)]
pub struct MessageState {
    /// Read: marked by opening the message, or through the mailbox's
    /// verbs, never an undo step of its own.
    #[model(readonly)]
    pub read: bool,
    /// Starred
    pub starred: bool,
    /// Folder
    pub folder: Folder,
    /// Labels
    pub labels: Set<String>,
    /// Where it lives on its mail server and what that server holds; none
    /// for a message only here (a draft, a sent copy, the sample).
    #[model(readonly)]
    pub server: Option<ServerCopy>,
    /// Deleted for good: a delayed fetch never brings it back.
    #[model(readonly)]
    pub purged: bool,
}

impl MessageState {
    fn valid(&self) -> Result<(), Refusal> {
        if self
            .labels
            .iter()
            .any(|label| label.trim().is_empty() || label.chars().count() > MAX_LABEL_CHARS)
        {
            return Err(Refusal::new("a label is a short, non-blank name"));
        }
        Ok(())
    }
}

/// A message as its mail server last reported or confirmed it.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct ServerCopy {
    /// The account it is fetched through
    pub account: String,
    /// Its `Message-ID`, which finds it on the server
    pub locator: String,
    /// Read there
    pub read: bool,
    /// Starred there
    pub starred: bool,
    /// The folder it is in there
    pub folder: Folder,
    /// Deleted there for good
    pub removed: bool,
}

/// A folder the reader made.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = LocalFolder::valid)]
pub struct LocalFolder {
    /// Name
    #[model(label)]
    pub name: String,
}

impl LocalFolder {
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_FOLDER_NAME_CHARS {
            Err(Refusal::new("a folder has a short, non-blank name"))
        } else {
            Ok(())
        }
    }
}

/// The message being written, as saved.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = DraftState::valid)]
pub struct DraftState {
    /// The account it is sent from, when one was chosen for it.
    pub sender: Option<DraftSender>,
    /// To
    pub to: String,
    /// Cc
    pub cc: String,
    /// Bcc
    pub bcc: String,
    /// Subject
    pub subject: String,
    /// The body as plain text: the text alternative it is sent with.
    pub body: String,
    /// The body as HTML, when it was written formatted.
    pub html_body: Option<String>,
    /// An iCalendar invitation it carries.
    pub calendar_ics: Option<String>,
    /// Its attachments, by content id.
    pub attachments: Vec<DraftAttachment>,
    /// The body being written, once it was started.
    pub rich_body: Option<RichText>,
}

impl DraftState {
    fn valid(&self) -> Result<(), Refusal> {
        let within = |text: &str, max: usize| text.chars().count() <= max;
        if ![&self.to, &self.cc, &self.bcc]
            .iter()
            .all(|field| within(field, MAX_DRAFT_ADDRESS_CHARS))
            || !within(&self.subject, MAX_DRAFT_SUBJECT_CHARS)
            || !within(&self.body, MAX_DRAFT_BODY_CHARS)
            || !self
                .html_body
                .as_deref()
                .is_none_or(|html| within(html, MAX_DRAFT_BODY_CHARS))
        {
            return Err(Refusal::new("the draft is too long"));
        }
        if self
            .calendar_ics
            .as_deref()
            .is_some_and(|ics| !valid_calendar_invitation(ics))
        {
            return Err(Refusal::new("the invitation is not a calendar request"));
        }
        if crate::attachment_store::bounded(&self.attachments) != self.attachments {
            return Err(Refusal::new(
                "the attachments are not ones the draft can carry",
            ));
        }
        Ok(())
    }

    /// The draft as the composer and the engine read it.
    pub fn draft(&self) -> Draft {
        Draft {
            sender: self.sender.clone(),
            to: self.to.clone(),
            cc: self.cc.clone(),
            bcc: self.bcc.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            html_body: self.html_body.clone(),
            calendar_ics: self.calendar_ics.clone(),
            attachments: self.attachments.clone(),
            in_reply_to: None,
            wire: crate::model::WireIds::default(),
        }
    }

    /// This draft saved as `draft` says, its body being written kept.
    #[must_use]
    pub fn saved(&self, draft: &Draft) -> Self {
        let data = DraftData::from_draft(draft);
        Self {
            sender: data.sender,
            to: data.to,
            cc: data.cc,
            bcc: data.bcc,
            subject: data.subject,
            body: data.body,
            html_body: data.html_body,
            calendar_ics: data.calendar_ics,
            attachments: data.attachments,
            rich_body: self.rich_body.clone(),
        }
    }

    /// Whether it holds anything: a recipient, a subject, a body, an
    /// invitation, an attachment or a chosen sender.
    pub fn is_empty(&self) -> bool {
        self.draft().is_empty()
    }
}

#[resource(
    kind = "mail.mailbox",
    label = "Mail",
    params = mailbox,
    children = Mail::places,
    search = Mail::search,
    scopes = Mail::scopes,
    queue = Mail::queue,
)]
impl Mail {
    /// Mark messages read
    ///
    /// Marks messages read or unread together.
    #[action(housekeeping)]
    pub fn mark_read(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
        #[param("Read, or unread")] read: bool,
    ) -> Result<(), Refusal> {
        for id in self.present(&messages)? {
            if let Some(message) = self.messages.get_mut(&id) {
                message.read = read;
            }
        }
        Ok(())
    }

    /// Archive messages
    ///
    /// Moves messages to the Archive together.
    #[action]
    pub fn archive(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, Folder::Archive)
    }

    /// Move messages to Trash
    ///
    /// Moves messages to the Trash together, from where they can still be
    /// brought back.
    #[action]
    pub fn trash(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, Folder::Trash)
    }

    /// Move messages
    ///
    /// Moves messages to one folder together.
    #[action]
    pub fn move_messages(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
        #[param("The folder")] to: Folder,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, to)
    }

    /// Delete permanently
    ///
    /// Deletes messages already in the Trash for good: all of them, or none
    /// when one is no longer there.
    #[action(destructive)]
    pub fn purge(
        &mut self,
        #[param("Ids of the messages, every one in the Trash")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        let ids = self.present(&messages)?;
        if ids.iter().any(|id| {
            self.messages
                .get(id)
                .is_none_or(|message| message.folder != Folder::Trash)
        }) {
            return Err(Refusal::new(
                "every message must still be present in Trash before permanent deletion",
            ));
        }
        for id in ids {
            if let Some(message) = self.messages.get_mut(&id) {
                message.purged = true;
            }
        }
        Ok(())
    }

    /// Messages fetched or found: each joins the mailbox with the flags its
    /// source reports, and a fetched one records what its server holds. What
    /// the mailbox already says wins, except where the server changed since
    /// it last reported (another mail client read the message).
    #[action(external)]
    pub fn ingest(
        &mut self,
        #[param("The messages, with what their source reports")] messages: Vec<IngestBaseline>,
    ) {
        for baseline in messages {
            self.ingest_one(&baseline);
        }
    }

    /// A mail server confirmed it holds what `update` asked of it.
    #[action(external)]
    pub fn server_applied(&mut self, #[param("What the server was told")] update: ServerUpdate) {
        let Some(message) = self.messages.get_mut(&Id::from_raw(update.id())) else {
            return;
        };
        let Some(server) = message.server.as_mut() else {
            return;
        };
        match update {
            ServerUpdate::Flags { read, starred, .. } => {
                server.read = read;
                server.starred = starred;
            }
            ServerUpdate::Move { to, .. } => server.folder = to,
            ServerUpdate::Remove { .. } => server.removed = true,
        }
    }

    /// Start the body of the draft afresh: a blank message, a reply's quote,
    /// a reopened draft.
    #[action]
    pub fn start_draft_body(
        &mut self,
        #[param("Its text, a paragraph per line; a line starting with > is quoted")] text: String,
        #[param("Its HTML, when it starts formatted")] html: Option<String>,
    ) {
        let mut body = RichText::default();
        let html = html
            .filter(|html| !html.trim().is_empty())
            .unwrap_or_else(|| quoted_html(&text));
        body.perform(RichTextAction::ReplaceFormatted {
            format: tilo_document::action::ImportFormat::Html,
            bytes: bounded_text(&html, MAX_DRAFT_BODY_CHARS, true).into_bytes(),
        });
        self.draft.rich_body = Some(body);
    }

    /// Save draft
    ///
    /// Saves the message being written as the draft: who it goes to, what
    /// it says and carries, keeping the body being written.
    #[action]
    pub fn save_draft(&mut self, #[param("The message as written")] message: DraftData) {
        self.draft = self.draft.saved(&message.to_draft());
    }

    /// Attach to draft
    ///
    /// Attaches a file another step handed over (a file from Files, a
    /// picture) to the message being written, under the name given. Save
    /// the draft's text first: saving the draft again keeps only the
    /// attachments it names. At most 16 attachments of 32 MiB each.
    #[action(label = "Attach to draft", prepare = prepare_attachment)]
    pub fn attach(
        &mut self,
        #[param("The file another step handed over, by its content address", content)]
        content: String,
        #[param(
            "The attachment's file name, with its extension",
            example = "report.pdf"
        )]
        name: String,
        #[param("Filled in from the content where it is attached; leave it out")] size: Option<u64>,
        #[param("Filled in from the name where it is attached; leave it out")] mime: Option<String>,
    ) -> Result<(), Refusal> {
        let attachment = DraftAttachment {
            content_id: content,
            filename: name,
            mime: mime.ok_or("the attachment's type is filled in where it is attached")?,
            size: size.ok_or("the attachment's size is filled in where it is attached")?,
        };
        if !crate::attachment_store::is_valid(&attachment) {
            return Err(Refusal::new("the attachment is not one a draft can carry"));
        }
        let mut attachments = self.draft.attachments.clone();
        attachments.push(attachment);
        if crate::attachment_store::bounded(&attachments) != attachments {
            return Err(Refusal::new(
                "the draft already carries this file, or as many or as large attachments as \
                 a message can",
            ));
        }
        self.draft.attachments = attachments;
        Ok(())
    }

    /// Discard draft
    #[action(destructive)]
    pub fn discard_draft(&mut self) {
        self.draft = DraftState::default();
    }

    /// Send message
    ///
    /// Puts the message in the outbox; a device that holds the account
    /// delivers it. Until then, undo takes it back unsent.
    #[action(destructive)]
    pub fn send_message(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Outgoing>,
        #[param("The address it is sent from: one of the mailbox's accounts")] account: String,
        #[param("The message")] message: DraftData,
        #[param("Id of the message it answers")] in_reply_to: Option<String>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        let outgoing = Outgoing {
            id: id.as_str().to_owned(),
            account: bounded_text(&account, MAX_DRAFT_ADDRESS_CHARS, false),
            draft: DraftData::from_draft(&message.to_draft()),
            in_reply_to,
        };
        if !outgoing.is_valid() {
            return Err(Refusal::new(
                "a message needs a sending account and a recipient",
            ));
        }
        outbox.push(id, outgoing);
        Ok(())
    }

    /// Cancel sending
    ///
    /// Takes a message out of the outbox unsent: one still waiting, or one
    /// its server refused.
    #[action]
    pub fn cancel_send(
        &mut self,
        outbox: &mut Outbox<Outgoing>,
        #[param("The outbox message")] id: String,
    ) -> Result<(), Refusal> {
        let key = Id::from_raw(id);
        match outbox.get(&key).map(|entry| &entry.settled) {
            None => Err(Refusal::new("no such message in the outbox")),
            Some(Some(Settled::Done(()))) => Err(Refusal::new("the message was already sent")),
            Some(_) => {
                outbox.forget(&key);
                Ok(())
            }
        }
    }
}

/// Stage the content an attachment names where it is attached: read it from
/// this device's content store and keep it under the draft's attachment
/// identity, with its size and type, so every replica applies the same
/// attachment and the bytes are verified before the draft names them.
fn prepare_attachment(params: &mut mailbox::Attach) -> Result<(), String> {
    let address = params.content.trim();
    let hash = ui::tilo_actions::content_hash(address)
        .or_else(|| address.strip_prefix(crate::attachment_store::PREFIX))
        .unwrap_or(address);
    if !tilo_content::valid_hash(hash) {
        return Err(format!(
            "{address:?} is not a content address: attach what another step handed over"
        ));
    }
    let store = tilo_resource::content::current()
        .ok_or("this window has no content store to attach from")?;
    let bytes = store
        .read(hash)
        .ok_or("the handed-over file is not on this device; hand it over again here")?;
    let staged = crate::attachment_store::stage_bytes(&store, &bytes, &params.name)?;
    params.content = staged.content_id;
    params.name = staged.filename;
    params.size = Some(staged.size);
    params.mime = Some(staged.mime);
    Ok(())
}

/// `text` as HTML: each line a paragraph, a line starting with `> `
/// quoted, nothing in it taken as markup.
fn quoted_html(text: &str) -> String {
    text.split('\n')
        .map(|line| match line.strip_prefix("> ") {
            Some(quoted) => format!("<blockquote><p>{}</p></blockquote>", escape_html(quoted)),
            None => format!("<p>{}</p>", escape_html(line)),
        })
        .collect()
}

fn escape_html(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

impl Mail {
    /// The ids of `messages` the mailbox holds and has not deleted for
    /// good, at most [`MAX_MESSAGES_PER_ACTION`] of them; refused when none
    /// is.
    fn present(&self, messages: &[String]) -> Result<Vec<Id>, Refusal> {
        let mut ids: Vec<Id> = Vec::new();
        for id in messages {
            let id = id.trim();
            if id.is_empty() || id.chars().count() > MAX_MESSAGE_ID_CHARS {
                continue;
            }
            let id = Id::from_raw(id);
            if ids.len() < MAX_MESSAGES_PER_ACTION
                && !ids.contains(&id)
                && self
                    .messages
                    .get(&id)
                    .is_some_and(|message| !message.purged)
            {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            Err(Refusal::new("none of these messages is in the mailbox"))
        } else {
            Ok(ids)
        }
    }

    fn move_to(&mut self, messages: &[String], folder: Folder) -> Result<(), Refusal> {
        for id in self.present(messages)? {
            if let Some(message) = self.messages.get_mut(&id) {
                message.folder = folder;
            }
        }
        Ok(())
    }

    fn ingest_one(&mut self, baseline: &IngestBaseline) {
        if baseline.id.is_empty() || baseline.id.chars().count() > MAX_MESSAGE_ID_CHARS {
            return;
        }
        let folder = baseline.folder.as_deref().and_then(Folder::from_key);
        let server = match (&baseline.account, &baseline.locator) {
            (Some(account), Some(locator)) => Some((account.clone(), locator.clone())),
            _ => None,
        };
        let key = Id::from_raw(baseline.id.clone());
        let Some(message) = self.messages.get_mut(&key) else {
            let state = MessageState {
                read: baseline.read.unwrap_or(false),
                starred: baseline.starred.unwrap_or(false),
                folder: folder.unwrap_or_default(),
                labels: Set::new(),
                server: server.map(|(account, locator)| ServerCopy {
                    account,
                    locator,
                    read: baseline.read.unwrap_or(false),
                    starred: baseline.starred.unwrap_or(false),
                    folder: folder.unwrap_or_default(),
                    removed: false,
                }),
                purged: false,
            };
            self.messages.push(key, state);
            return;
        };
        if message.purged {
            return;
        }
        let Some((account, locator)) = server else {
            return;
        };
        let Some(known) = message.server.as_mut() else {
            message.server = Some(ServerCopy {
                account,
                locator,
                read: baseline.read.unwrap_or(message.read),
                starred: baseline.starred.unwrap_or(message.starred),
                folder: folder.unwrap_or(message.folder),
                removed: false,
            });
            return;
        };
        known.account = account;
        known.locator = locator;
        if let Some(read) = baseline.read.filter(|read| *read != known.read) {
            known.read = read;
            message.read = read;
        }
        if let Some(starred) = baseline.starred.filter(|starred| *starred != known.starred) {
            known.starred = starred;
            message.starred = starred;
        }
        if let Some(folder) = folder.filter(|folder| *folder != known.folder) {
            known.folder = folder;
            message.folder = folder;
        }
    }

    /// What the mailbox holds of the message `id`, unless it was deleted
    /// for good.
    pub fn message(&self, id: &str) -> Option<&MessageState> {
        self.messages
            .get(&Id::from_raw(id))
            .filter(|message| !message.purged)
    }

    /// What every fetched message's server still has to be told: the
    /// mailbox's flags, folder and permanent deletions where the server
    /// differs, in the order a server applies them (flags in the folder the
    /// message is in, then the move).
    /// After every change, whoever made it: queue telling each message's
    /// server what it does not hold yet, and forget what it no longer has
    /// to be told (an undo that took the change back first).
    fn queue(&mut self, _before: &Self, cx: &mut ActionCx) {
        let wanted = self.server_updates();
        let stale: Vec<Id> = self
            .server
            .pending()
            .filter(|(_, update)| !wanted.contains(update))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.server.forget(&id);
        }
        for update in wanted {
            if !self.server.pending().any(|(_, queued)| *queued == update) {
                self.server.push(cx.id(), update);
            }
        }
        self.server.prune(SETTLED_KEPT);
        self.outbox.prune(SETTLED_KEPT);
    }

    pub fn server_updates(&self) -> Vec<ServerUpdate> {
        let mut updates = Vec::new();
        for (id, message) in self.messages.iter() {
            let Some(server) = &message.server else {
                continue;
            };
            let (id, account, locator) = (
                id.as_str().to_owned(),
                server.account.clone(),
                server.locator.clone(),
            );
            if message.purged {
                if !server.removed {
                    updates.push(ServerUpdate::Remove {
                        id,
                        account,
                        locator,
                        folder: server.folder,
                    });
                }
                continue;
            }
            if message.read != server.read || message.starred != server.starred {
                updates.push(ServerUpdate::Flags {
                    id: id.clone(),
                    account: account.clone(),
                    locator: locator.clone(),
                    folder: server.folder,
                    read: message.read,
                    starred: message.starred,
                });
            }
            if message.folder != server.folder {
                updates.push(ServerUpdate::Move {
                    id,
                    account,
                    locator,
                    from: server.folder,
                    to: message.folder,
                });
            }
        }
        updates
    }

    /// Show the reader's flags, folders and labels on the loaded mail, and
    /// drop what was deleted for good. The mail's content is the engine's;
    /// what the reader did with it is the mailbox's.
    /// Whether the record knows message `id`.
    pub(crate) fn knows(&self, id: &str) -> bool {
        self.messages.get(&Id::from_raw(id.to_owned())).is_some()
    }

    pub fn apply_flags(&self, mailbox: &mut crate::model::Mailbox) {
        let ids: Vec<_> = mailbox.messages.keys().copied().collect();
        for id in ids {
            let Some(state) = self.messages.get(&Id::from_raw(id.to_string())) else {
                continue;
            };
            if state.purged {
                mailbox.purge(id);
                continue;
            }
            let labels: Vec<String> = state.labels.iter().cloned().collect();
            let changed = mailbox.get(id).is_some_and(|message| {
                message.read != state.read
                    || message.starred != state.starred
                    || message.folder != state.folder
                    || message.labels != labels
            });
            // Only a message that differs is changed, so an unchanged
            // mailbox keeps its revision and what is derived from it.
            if changed && let Some(message) = mailbox.get_mut(id) {
                message.read = state.read;
                message.starred = state.starred;
                message.folder = state.folder;
                message.labels = labels;
            }
        }
    }

    /// Messages sent and not delivered, with the reason its server refused
    /// one that it did.
    pub fn outgoing(&self) -> Vec<(Outgoing, Option<String>)> {
        self.outbox
            .entries()
            .filter_map(|(_, entry)| match &entry.settled {
                None => Some((entry.value.clone(), None)),
                Some(Settled::Failed { reason, .. }) => {
                    Some((entry.value.clone(), Some(reason.clone())))
                }
                Some(Settled::Done(())) => None,
            })
            .collect()
    }

    /// The folders the reader made, as `(id, name)`.
    pub fn custom_folders(&self) -> Vec<(String, String)> {
        self.folders
            .iter()
            .map(|(id, folder)| (id.as_str().to_owned(), folder.name.clone()))
            .collect()
    }

    /// Whether a draft is saved, or its body was started.
    pub fn has_draft(&self) -> bool {
        !self.draft.is_empty() || self.draft.rich_body.is_some()
    }

    /// Every folder is a part of the mailbox search can be narrowed to.
    fn scopes(&self) -> Vec<ui::search::ScopeDecl> {
        let mut scopes = Folder::ALL
            .into_iter()
            .map(|folder| {
                ui::search::ScopeDecl::new(
                    format!("mail.folder:{}", folder.as_key()),
                    folder.title(),
                )
            })
            .collect::<Vec<_>>();
        scopes.extend(self.folders.iter().map(|(id, folder)| {
            ui::search::ScopeDecl::new(format!("mail.folder:{id}"), folder.name.clone())
        }));
        scopes
    }

    /// The folders, each opening its place.
    fn search(&self) -> Vec<SearchEntry> {
        crate::search::folder_entries(&self.custom_folders())
    }

    /// Each folder as a place, the messages in it (as many as the mailbox
    /// publishes) beneath it; the draft beneath Drafts.
    fn places(&self) -> Vec<Derived<Self>> {
        let mut remaining = OPEN_MESSAGE_LIMIT;
        let mut places = Vec::new();
        for folder in Folder::ALL {
            let mut place = Derived::new(folder.title())
                .of_kind("folder")
                .identified(folder_place_id(folder.as_key()))
                .with_effect(&MailEffect::Open {
                    target: MailTarget::Folder(folder.as_key().to_owned()),
                });
            if folder == Folder::Drafts && self.has_draft() {
                place = place.with_child(
                    Derived::new("Current draft")
                        .of_kind("draft")
                        .identified("draft")
                        .with_effect(&MailEffect::Open {
                            target: MailTarget::Draft,
                        }),
                );
            }
            for (id, message) in self
                .messages
                .iter()
                .filter(|(_, message)| !message.purged && message.folder == folder)
                .take(remaining)
            {
                place = place.with_child(Self::message_place(id.as_str(), message));
                remaining = remaining.saturating_sub(1);
            }
            places.push(place);
        }
        for (id, folder) in self.folders.iter() {
            places.push(
                Derived::new(folder.name.clone())
                    .of_kind("folder")
                    .identified(folder_place_id(id.as_str())),
            );
        }
        places
    }

    /// The message `id` as a place: opening, previewing and answering it,
    /// marking it read, archiving or deleting it.
    pub(crate) fn message_place(id: &str, message: &MessageState) -> Derived<Self> {
        let ids = vec![id.to_owned()];
        let mut place = Derived::new(id)
            .of_kind("message")
            .identified(message_place_id(id))
            .with_effect(&MailEffect::Open {
                target: MailTarget::Message(id.to_owned()),
            })
            .with_effect(&MailEffect::Inspect {
                message: id.to_owned(),
            })
            .with_effect(&MailEffect::ReadMessage {
                message: id.to_owned(),
            })
            .with_effect(&MailEffect::Reply {
                message: id.to_owned(),
            })
            .with_effect(&MailEffect::ReplyAll {
                message: id.to_owned(),
            })
            .with_effect(&MailEffect::Forward {
                message: id.to_owned(),
            })
            .with(
                Self::MARK_READ
                    .bind(mailbox::MarkRead::paths().messages, ids.clone())
                    .and(mailbox::MarkRead::paths().read, !message.read),
            )
            .with(Self::ARCHIVE.bind(mailbox::Archive::paths().messages, ids.clone()));
        place = if message.folder == Folder::Trash {
            place.with(Self::PURGE.bind(mailbox::Purge::paths().messages, ids))
        } else {
            place.with(Self::TRASH.bind(mailbox::Trash::paths().messages, ids))
        };
        place
    }
}

/// The id of the folder `key`'s place among the mailbox's.
pub(crate) fn folder_place_id(key: &str) -> String {
    format!("folder:{key}")
}

/// The id of the message `id`'s place beneath its folder's.
pub(crate) fn message_place_id(id: &str) -> String {
    format!("message:{id}")
}

/// The published id of the folder `key`'s place.
pub fn folder_resource(key: &str) -> String {
    format!(
        "{}/derived/{}",
        <Mail as tilo_resource::Resource>::KIND,
        folder_place_id(key)
    )
}

/// The mailbox `doc` holds, when it reads as one.
pub(crate) fn read_mail(doc: &tilo_resource::Doc) -> Option<Mail> {
    <Mail as tilo_resource::Resource>::read(doc).ok()
}

/// Where the body of the draft is in the mailbox.
pub(crate) fn draft_body() -> tilo_resource::Path<Mail, Option<RichText>> {
    Mail::paths().draft.rich_body
}

/// The object the draft's body is written in, once it was started.
pub(crate) fn draft_body_object(doc: &tilo_resource::Doc) -> Option<ui::automerge::ObjId> {
    draft_body().object(doc)
}

/// The dispatcher the composer edits the draft body through: each edit is
/// the mailbox's field edit of the body.
pub(crate) fn draft_body_dispatcher(dispatcher: &Dispatcher<Mail>) -> Dispatcher<RichText> {
    let body = draft_body().some();
    dispatcher.child::<RichText>(
        &tilo_resource::child_resource_id::<Mail>("draft-body"),
        move |op| tilo_resource::Stamped {
            operation: MailAction::FieldEdit(body.edit(tilo_resource::EditOp::Rich {
                rich: serde_json::to_value(&op).unwrap_or_default(),
            })),
            now_ms: 0,
        },
    )
}

/// What a test reads of the mailbox its document holds, and the edits it
/// sets the mailbox up with.
#[cfg(test)]
pub(crate) struct TestMail;

#[cfg(test)]
impl TestMail {
    /// A new mailbox's document.
    pub(crate) fn doc() -> tilo_resource::Doc {
        tilo_resource::Replica::<Mail>::new().doc().clone()
    }

    fn mail(doc: &tilo_resource::Doc) -> Mail {
        read_mail(doc).unwrap_or_default()
    }

    fn change(doc: &mut tilo_resource::Doc, submit: tilo_resource::Invoke<Mail>) {
        let mut mail = tilo_resource::Replica::<Mail>::from_doc(doc.clone());
        mail.apply_confirmed(submit)
            .expect("a test's change applies");
        *doc = mail.doc().clone();
    }

    pub(crate) fn get_read(doc: &tilo_resource::Doc, id: &str) -> Option<bool> {
        Self::mail(doc).message(id).map(|message| message.read)
    }

    pub(crate) fn get_starred(doc: &tilo_resource::Doc, id: &str) -> Option<bool> {
        Self::mail(doc).message(id).map(|message| message.starred)
    }

    pub(crate) fn get_folder_for(doc: &tilo_resource::Doc, id: &str) -> Option<Folder> {
        Self::mail(doc).message(id).map(|message| message.folder)
    }

    pub(crate) fn is_present_message(doc: &tilo_resource::Doc, id: &str) -> bool {
        Self::mail(doc).message(id).is_some()
    }

    pub(crate) fn server_updates(doc: &tilo_resource::Doc) -> Vec<ServerUpdate> {
        Self::mail(doc).server_updates()
    }

    pub(crate) fn load_draft(doc: &tilo_resource::Doc) -> Draft {
        Self::mail(doc).draft.draft()
    }

    pub(crate) fn outbox(doc: &tilo_resource::Doc) -> Vec<(Outgoing, Option<String>)> {
        Self::mail(doc).outgoing()
    }

    pub(crate) fn apply_flags(doc: &tilo_resource::Doc, mailbox: &mut crate::model::Mailbox) {
        Self::mail(doc).apply_flags(mailbox);
    }

    pub(crate) fn draft_body(doc: &tilo_resource::Doc) -> Option<ui::automerge::ObjId> {
        draft_body_object(doc)
    }

    pub(crate) fn store_draft(doc: &mut tilo_resource::Doc, draft: &Draft) {
        Self::change(
            doc,
            Mail::SAVE_DRAFT.with(mailbox::SaveDraft {
                message: DraftData::from_draft(draft),
            }),
        );
    }

    pub(crate) fn clear_draft(doc: &mut tilo_resource::Doc) {
        Self::change(doc, Mail::DISCARD_DRAFT.with(mailbox::DiscardDraft {}));
    }

    /// Apply what `dispatcher` asked the person to confirm, as the door
    /// does once they agree.
    pub(crate) fn confirm(doc: &mut tilo_resource::Doc, dispatcher: &Dispatcher<Mail>) {
        let mut mail = tilo_resource::Replica::<Mail>::from_doc(doc.clone());
        for action in dispatcher.take_asked().expect("what was asked builds") {
            mail.apply_action(&action)
                .expect("a confirmed change applies");
        }
        *doc = mail.doc().clone();
    }

    /// Record how the delivery of the outbox message `id` settled, as the
    /// outbox drain does.
    pub(crate) fn settle_delivery(doc: &mut tilo_resource::Doc, id: &str, settled: &Settled) {
        Self::change(
            doc,
            tilo_resource::Invoke::from(Mail::paths().outbox.settle(&Id::from_raw(id), settled)),
        );
    }

    pub(crate) fn set_folder_for(doc: &mut tilo_resource::Doc, id: &str, folder: Folder) {
        Self::change(
            doc,
            Mail::INGEST.with(mailbox::Ingest {
                messages: vec![IngestBaseline::register_only(id)],
            }),
        );
        Self::change(
            doc,
            tilo_resource::Invoke::edit(
                &Mail::paths()
                    .messages
                    .item(&Id::from_raw(id))
                    .folder
                    .set(folder),
            )
            .expect("a folder edit"),
        );
    }
}

/// Hosted-only extras that are not document state: an in-flight send.
pub(crate) fn live_extras(app: &MailApp) -> Vec<tilo_actions::Resource> {
    let mut extras = Vec::new();
    if app.has_pending_send() {
        extras.push(tilo_actions::Resource::new(
            "mail.send:pending",
            "mail.send",
            "Sending message…",
        ));
    }
    extras
}

/// Mail's preferences, which follow the person across their devices and are
/// never seen by collaborators: the format a new message is written in and
/// the accounts whose folders the sidebar keeps folded, and the senders
/// whose remote images load.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailPreferences {
    #[serde(default)]
    pub compose_format: crate::model::ComposeFormat,
    #[serde(default)]
    pub collapsed_accounts: Vec<String>,
    /// The senders whose remote images load without asking.
    #[serde(default)]
    pub remote_content_senders: Vec<String>,
}

impl ui::storage::SettingsSchema for MailPreferences {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

/// Framework app: the mail widget is the surface over [`Mail`].
pub struct MailShell;

/// Every screenshot scenario. Each shows the sample mailbox and accounts
/// (`firstrun` none, `empty` an account with no mail yet) with the view state
/// its name says, staged by the surface.
const SCENARIOS: &[&str] = &[
    "wide",
    "narrow",
    "narrow_detail",
    "narrow_drawer",
    "mid",
    "compose",
    "compose_narrow",
    "setup",
    "setup_narrow",
    "setup_add",
    "setup_blank",
    "setup_discovering",
    "setup_oauth",
    "firstrun",
    "html_mail",
    "search",
    "preview",
    "bulk",
    "bulk_purge_confirmation",
    "reply",
    "context_menu",
    "loading",
    "empty",
    "offline",
    "error",
];

/// What the binary runs instead of opening a window.
///
/// `--probe-accounts-engine VALUE` and `--probe-contact-directory-engine
/// VALUE` (the value is ignored) run the self-checks of account custody with
/// the IMAP client, and of the contact directory, without a network or an
/// account. They need `runtime-engine`, the release build, where that code is
/// in `libtilo_accounts_engine.so` and `libtilo_contact_directory_engine.so`;
/// the checks prove Mail loads those libraries and runs them.
const COMMANDS: &[ui::framework::Command] = &[
    ui::framework::Command {
        option: "--probe-accounts-engine",
        run: probe_accounts_engine,
    },
    ui::framework::Command {
        option: "--probe-contact-directory-engine",
        run: probe_contact_directory_engine,
    },
];

#[cfg(feature = "runtime-engine")]
fn probe_accounts_engine(_: &str) -> Result<(), String> {
    let report = tilo_accounts_engine::self_check()?;
    println!("mail --probe-accounts-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_accounts_engine(_: &str) -> Result<(), String> {
    Err("the accounts engine library needs the `runtime-engine` feature".to_string())
}

#[cfg(feature = "runtime-engine")]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    let report = tilo_contact_directory_engine::self_check()?;
    println!("mail --probe-contact-directory-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    Err("the contact directory's engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for MailShell {
    type Resource = Mail;
    const ID: &'static str = "so.tilo.mail";
    const TITLE: &'static str = "Mail";
    const ICON: Option<&'static str> = Some("mail");

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    /// Only a live window reaches mail servers and the account store; History
    /// shows what each past version of the document held, and a screenshot
    /// its scenario's fixture.
    fn view(cx: &mut ViewCx<Mail>) -> Element {
        let app = match cx.mode() {
            RunMode::Live => MailApp::for_window(cx.storage(), cx.content()),
            RunMode::Past => MailApp::for_past_version(),
            RunMode::Capture => {
                let fixture = cx
                    .fixture::<MailFixture>()
                    .unwrap_or_else(|| std::rc::Rc::new(MailFixture::without_accounts()));
                MailApp::for_capture(&fixture)
            }
        };
        // A search hit, or an agent's `open`, shows the folder or message.
        cx.on_open(app.opener());
        cx.surface(app)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some(
                "narrow" | "narrow_detail" | "narrow_drawer" | "compose_narrow" | "setup_narrow",
            ) => (560, 860),
            Some("mid") => (1000, 720),
            Some("compose" | "reply") => (1180, 820),
            Some(
                "setup" | "setup_add" | "setup_blank" | "setup_discovering" | "setup_oauth"
                | "firstrun" | "error",
            ) => (1100, 780),
            Some("html_mail") => (1280, 820),
            _ => (1280, 800),
        }
    }

    fn fixture(fixture: &mut Fixture<Mail>) {
        let mail = match fixture.scenario() {
            Some("firstrun") => MailFixture::without_accounts(),
            Some("empty") => MailFixture::before_first_fetch(),
            _ => MailFixture::bundled(),
        };
        fixture.provide(mail);
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(crate::i18n::catalog())
    }
}

/// Whether an account set up on this device sends from `address`.
fn holds_account(state: &crate::state::MailState, address: &str) -> bool {
    state
        .accounts
        .accounts
        .iter()
        .any(|account| account.config.email == address)
}

impl ResourceSurface<Mail, MailEffect> for MailApp {
    /// Every message already loaded, outside Trash: its subject, sender and
    /// body come from the engine, not the document.
    fn search_entries(&self) -> Vec<SearchEntry> {
        let mut entries = crate::search::message_entries(&self.shared_mailbox());
        entries.extend(self.contact_recipient_search_entries());
        entries
    }

    fn present(&mut self, state: &std::rc::Rc<Mail>) {
        self.has_contact_draft_conflict = state.has_draft();
        // Shared folder/message Document state is adopted through sync from
        // the Automerge document. An empty projection must not wipe the
        // unpublished local mailbox corpus or in-flight compose.
    }

    /// Every change the document records that a mail server does not hold
    /// yet — whoever made it — goes to the server from a live window on a
    /// device with the account, and the engine's acknowledgement records it
    /// as applied.
    fn attach(&mut self, cx: SurfaceCx<Mail>) {
        self.attach_framework(cx.dispatcher());
        // A screenshot shows its fixture as it is, whatever the profile holds.
        if cx.mode().is_live() {
            self.wake_from_workers(cx.waker());
            self.follow_preferences(cx.settings::<MailPreferences>());
            self.contact_directory = cx.shared_person::<tilo_contact_directory::ContactDirectory>();
            if let Some(accounts) = cx.shared_person::<accounts::person::PersonAccounts>() {
                self.follow_person_accounts(accounts);
            }
        }
        let outbox = self.server_outbox();
        let shared = std::rc::Rc::clone(&self.shared);
        let projection = cx.outbox(
            Mail::paths().server,
            move |_, update: &ServerUpdate| holds_account(&shared.borrow(), update.account()),
            move |_, update: ServerUpdate, cx| {
                let reply = cx.reply();
                outbox.borrow_mut().push(
                    update,
                    Box::new(move |result| {
                        reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
                    }),
                );
                Ok(Effected::Pending)
            },
        );
        self.follow_projection(projection);
        let outbox = self.server_outbox();
        let shared = std::rc::Rc::clone(&self.shared);
        let delivery = cx.outbox(
            Mail::paths().outbox,
            move |_, outgoing: &Outgoing| holds_account(&shared.borrow(), &outgoing.account),
            move |_, outgoing: Outgoing, cx| {
                let reply = cx.reply();
                outbox.borrow_mut().deliver(
                    outgoing,
                    Box::new(move |result| {
                        reply.finish(result.map(|settled| tilo_actions::ActionOutput {
                            value: serde_json::to_value(&settled).ok(),
                            resources: Vec::new(),
                        }));
                    }),
                );
                Ok(Effected::Pending)
            },
        );
        self.follow_delivery(delivery);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.request_stage(scenario);
        }
    }

    fn effect(&mut self, effect: MailEffect, cx: &mut EffectCx<Mail, Self>) -> EffectAnswer {
        if let MailEffect::Save { message, index } = &effect {
            let (name, bytes) = self.attachment_to_save(message, *index)?;
            let resource = format!("mail.attachment:{message}:{index}");
            let request = ui::portal::PickRequest::save("Save attachment", "Save", &name)
                .thread_name("mail-portal-save-attachment")
                .filter(ui::portal::FileFilter::all_files());
            return Ok(
                cx.save_file(request, bytes, move |app: &mut Self, saved, _| {
                    Ok(app.finish_attachment_save(saved, &name, resource))
                }),
            );
        }
        if matches!(effect, MailEffect::PrepareContactDraft { .. }) {
            let state = cx.live().get();
            self.has_contact_draft_conflict = state.has_draft();
        }
        let invocation = cx.invocation().unwrap_or_default();
        let answer = self.begin_effect(invocation, effect)?;
        if matches!(answer, Effected::Pending) {
            self.await_effect(invocation, cx.reply());
        }
        Ok(answer)
    }

    /// Fetching and preparing an invitation need a connected account.
    fn effects(&self) -> Vec<EffectStatus> {
        if self.can_sync() {
            return Vec::new();
        }
        vec![
            EffectStatus::unavailable(
                MailEffect::REFRESH,
                "account_offline",
                "No connected mail account is available",
            ),
            EffectStatus::unavailable(
                MailEffect::PREPARE_CALENDAR_INVITATION,
                "account_offline",
                "Connect a Mail account before preparing an invitation",
            ),
        ]
    }

    /// The accounts, an in-flight send and a search-index fault.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    /// A loaded message, thread, attachment or account the mailbox does not
    /// publish.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::OperationKind;
    use ui::tilo_resource::Replica;
    use ui::widget::Widget;

    /// A file another step handed over attaches to the draft by its
    /// content address: staged and verified where it is attached, carried as
    /// the same attachment on every replica, refused when it is not there.
    #[test]
    fn a_handed_over_file_attaches_to_the_draft() {
        let mut app = ui::framework::testing::TestApp::<MailShell>::new().unwrap();
        let hash = app.content().put(b"quarterly numbers").unwrap();
        app.call(
            "attach",
            serde_json::json!({
                "content": ui::tilo_actions::content_resource(&hash),
                "name": "numbers.csv",
            }),
        )
        .unwrap();
        let attachments = app.state().draft.attachments;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "numbers.csv");
        assert_eq!(attachments[0].size, 17);
        assert_eq!(
            crate::attachment_store::materialize(&app.content(), &attachments[0]).unwrap(),
            b"quarterly numbers"
        );
        assert!(
            app.call(
                "attach",
                serde_json::json!({"content": hash, "name": "again.csv"}),
            )
            .is_err(),
            "the same file is attached once"
        );
        let missing = tilo_content::hash_bytes(b"never stored");
        assert!(
            app.call(
                "attach",
                serde_json::json!({"content": missing, "name": "x.txt"}),
            )
            .is_err()
        );
        assert!(
            app.call(
                "attach",
                serde_json::json!({"content": "/home/you/x.txt", "name": "x.txt"}),
            )
            .is_err(),
            "a path is not content"
        );
        assert!(app.undo());
        assert!(app.state().draft.attachments.is_empty());
    }

    /// A loaded message reads as what it says, and reading it marks it read
    /// for no one.
    #[test]
    fn a_loaded_message_reads_as_what_it_says() {
        let mut app = ui::framework::testing::TestApp::<MailShell>::new().unwrap();
        let id: crate::model::MessageId = "70c4b1cf-78ef-bd2b-7019-d7c509f29930".parse().unwrap();
        app.with_surface(|mail: &mut MailApp| {
            mail.shared
                .borrow_mut()
                .mailbox
                .insert(crate::model::Message {
                    id,
                    rfc_message_id: Some("m7@example.test".into()),
                    in_reply_to: None,
                    from: crate::model::Addr::new("Sam", "sam@example.test"),
                    to: vec![crate::model::Addr::new("", "me@example.test")],
                    cc: Vec::new(),
                    subject: "Quarterly numbers".into(),
                    body: "They are up.".into(),
                    html_body: None,
                    attachments: Vec::new(),
                    timestamp_min: 0,
                    read: false,
                    starred: false,
                    folder: Folder::Inbox,
                    labels: Vec::new(),
                    account: String::new(),
                    server: None,
                });
        })
        .unwrap();
        app.settle();
        let row = format!("mail.message:{id}");
        let answer = app
            .effect_on(
                &row,
                &MailEffect::ReadMessage {
                    message: id.to_string(),
                },
            )
            .unwrap()
            .value
            .unwrap();
        assert_eq!(answer["subject"], "Quarterly numbers");
        assert_eq!(answer["from"], "Sam <sam@example.test>");
        assert_eq!(answer["text"], "They are up.");
        assert!(
            !app.with_surface(|mail: &mut MailApp| mail.shared.borrow().mailbox.messages[&id].read)
                .unwrap(),
            "reading it marks it read for no one"
        );
    }

    #[test]
    fn effects_reach_the_window_through_the_door() {
        let mut app = ui::framework::testing::TestApp::<MailShell>::new().unwrap();
        let surface = app.agent_surface().to_string();
        assert!(surface.contains("prepare-calendar-invitation"), "{surface}");
        app.effect(&MailEffect::Compose)
            .expect("composing needs no account");
        assert!(
            app.effect(&MailEffect::Refresh).is_err(),
            "nothing to refresh without a connected account"
        );
        app.effect_on(
            &folder_resource("archive"),
            &MailEffect::Open {
                target: crate::actions::MailTarget::Folder("archive".into()),
            },
        )
        .expect("a folder row opens its folder");
    }

    /// What the window adds beside the mailbox is its live rows, which
    /// never carry a document action, and an empty mailbox presented does not
    /// wipe the window's loaded mail.
    #[test]
    fn the_window_keeps_no_second_door() {
        let mut app = MailApp::new_sample();
        let mut doc = Replica::<Mail>::new().doc().clone();
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        let state = read_mail(&doc).expect("a mailbox");
        assert!(
            !state.messages.is_empty(),
            "sample sync ingests message flags into the mailbox"
        );
        let published = ui::tilo_resource::publish::<Mail>("mail.mailbox", "Mail", &state);
        assert_eq!(published.kind, "mail.mailbox");
        let inbox = published
            .children
            .iter()
            .find(|child| child.id.as_str() == folder_resource("inbox"))
            .expect("the inbox place on the published mailbox");
        let message = inbox
            .children
            .iter()
            .find(|child| child.kind == "mail.mailbox.message")
            .expect("a message place under its folder");
        for id in ["mark-read", "archive", "trash"] {
            assert!(
                message
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} rides the message place"
            );
        }
        let opened = message
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("a published message opens where it is");
        assert_eq!(opened.kind, OperationKind::View);

        for row in app.live_rows() {
            let row = row.into_resource();
            assert_ne!(row.kind, "mail.mailbox");
            assert!(
                row.actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "{}",
                row.id.as_str()
            );
        }

        let before = app.mailbox_message_count();
        assert!(before > 0, "sample app has a local mailbox corpus");
        <MailApp as ResourceSurface<Mail, MailEffect>>::present(
            &mut app,
            &std::rc::Rc::new(Mail::default()),
        );
        assert_eq!(
            app.mailbox_message_count(),
            before,
            "an empty mailbox does not clear unpublished local mail"
        );
    }

    mod captures {
        use ui::framework::ResourceApp as _;
        use ui::framework::testing::TestApp;

        use super::super::MailShell;
        use crate::model::Folder;

        /// Every scenario captures through the framework at its own
        /// viewport, from the bundled sample accounts rather than the
        /// person's.
        #[test]
        fn every_scenario_captures_the_fixture_at_its_viewport() {
            ui::i18n::I18nManager::init("en-US");
            for scenario in MailShell::scenarios() {
                let mut app = TestApp::<MailShell>::capture(Some(scenario))
                    .unwrap_or_else(|error| panic!("{scenario}: {error}"));
                let (width, height) = MailShell::shot_viewport(Some(scenario));
                assert_eq!(
                    app.harness().inspection().viewport,
                    [width, height],
                    "{scenario}"
                );
                let frame = app.harness().inspection_json().unwrap();
                match *scenario {
                    "firstrun" => assert!(frame.contains("No email account yet"), "{frame}"),
                    "setup" | "setup_narrow" => {
                        let email = crate::sample::sample_account_store().active_config().email;
                        assert!(frame.contains(&email), "{scenario}: {frame}");
                    }
                    "error" => assert!(frame.contains("That password didn’t work."), "{frame}"),
                    _ => {}
                }
            }
        }

        /// The window's one search source finds a loaded message by its
        /// subject and a folder by its name, and each hit opens a resource
        /// the window publishes.
        #[test]
        fn the_window_search_finds_loaded_mail_and_folders() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(None).unwrap();
            let message = crate::sample::sample_mailbox()
                .messages
                .values()
                .find(|message| message.folder == Folder::Inbox)
                .expect("the sample has inbox mail")
                .clone();
            let providers = ui::search::capability::search_providers(app.harness().root_widget());
            assert_eq!(providers.len(), 1, "one search source");
            let hits = providers[0].search(&message.subject);
            let hit = hits
                .iter()
                .find(|hit| hit.kind == crate::search::MAIL_KIND && hit.title == message.subject)
                .expect("the loaded message is found by its subject");
            assert!(
                hit.anchor().starts_with("mail.message:"),
                "{}",
                hit.anchor()
            );
            app.call("open", serde_json::json!({ "anchor": hit.anchor() }))
                .expect("the hit's message opens");
            app.settle();
            let folders = providers[0].search("Archive");
            let archive = folders
                .iter()
                .find(|hit| hit.kind == crate::search::FOLDER_KIND && hit.title == "Archive")
                .expect("the archive folder is found");
            app.call("open", serde_json::json!({ "anchor": archive.anchor() }))
                .expect("the hit's folder opens");
            app.settle();
            assert_eq!(
                app.with_surface::<crate::app::MailApp, _>(|mail| mail.shared.borrow().folder),
                Some(Folder::Archive)
            );
        }

        /// A message the engine loaded that has no row in the document yet
        /// still opens from a search hit: its address resolves to the
        /// window's live row and its open view.
        #[test]
        fn a_loaded_message_without_a_document_row_opens_from_a_hit() {
            ui::i18n::I18nManager::init("en-US");
            let app = crate::app::MailApp::new_sample();
            let message = app
                .shared_mailbox()
                .messages
                .values()
                .next()
                .expect("the sample has mail")
                .clone();
            let id = format!("mail.message:{}", message.id);
            let row = app
                .live_row(&id)
                .expect("a loaded message resolves to a live row")
                .into_resource();
            assert!(row.actions.iter().any(|action| action.id == "open"));
            assert!(
                app.live_row("mail.message:00000000-0000-0000-0000-000000000000")
                    .is_none(),
                "a message that is not loaded resolves to nothing"
            );
        }

        /// The purge confirmation shows messages that really are in Trash:
        /// staging moved them there through the resource's own action.
        #[test]
        fn the_purge_confirmation_stages_its_messages_in_trash_through_actions() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("bulk_purge_confirmation")).unwrap();
            let trashed = app
                .state()
                .messages
                .values()
                .filter(|message| !message.purged && message.folder == Folder::Trash)
                .count();
            let sample_trash = crate::sample::sample_mailbox()
                .messages
                .values()
                .filter(|message| message.folder == Folder::Trash)
                .count();
            assert_eq!(trashed, sample_trash + 2);
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("Delete permanently"), "{frame}");
        }

        /// A keyboard triage goes through the framework's door: the starred
        /// message is in the document, and one undo takes it back.
        #[test]
        fn a_shortcut_is_one_undo_step_through_the_framework() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let starred = |app: &TestApp<MailShell>| {
                app.state()
                    .messages
                    .values()
                    .filter(|message| message.starred)
                    .count()
            };
            let before = starred(&app);
            app.harness().type_text("s");
            let after = starred(&app);
            assert_ne!(after, before, "S toggled the selected message's star");
            assert!(app.undo());
            assert_eq!(starred(&app), before);
        }

        /// A change from any door — an agent's action, an undo, a
        /// collaborator's edit — is what the server has to be told, exactly
        /// as a click on the message is.
        #[test]
        fn every_door_reaches_the_server_through_the_document() {
            use crate::actions::{IngestBaseline, ServerUpdate};
            use crate::resource::{Mail, TestMail, mailbox};
            use ui::tilo_resource::Id;

            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let id = app
                .state()
                .messages
                .iter()
                .find(|(_, message)| {
                    message.folder == Folder::Inbox && !message.read && !message.starred
                })
                .map(|(id, _)| id.as_str().to_owned())
                .expect("an unread inbox message");
            let fetched = IngestBaseline {
                id: id.clone(),
                read: Some(false),
                starred: Some(false),
                folder: Some("inbox".into()),
                account: Some(crate::sample::sample_account_store().active_config().email),
                locator: Some("launch@example.test".into()),
            };
            app.invoke(Mail::INGEST.with(mailbox::Ingest {
                messages: vec![fetched],
            }))
            .unwrap();
            assert!(app.state().server_updates().is_empty());

            app.invoke(
                ui::tilo_resource::Invoke::edit(
                    &Mail::paths()
                        .messages
                        .item(&Id::from_raw(id.clone()))
                        .starred
                        .set(true),
                )
                .unwrap(),
            )
            .unwrap();
            assert!(matches!(
                app.state().server_updates().as_slice(),
                [ServerUpdate::Flags { starred: true, .. }]
            ));
            assert!(app.undo());
            assert!(
                app.state().server_updates().is_empty(),
                "undone before the server heard of it"
            );

            app.merge_peer_edit(|doc| TestMail::set_folder_for(doc, &id, Folder::Archive));
            assert!(matches!(
                app.state().server_updates().as_slice(),
                [ServerUpdate::Move {
                    to: Folder::Archive,
                    ..
                }]
            ));
        }

        /// A capture is the same on every run: the sample mailbox's ids do
        /// not change between captures.
        #[test]
        fn the_sample_mailbox_is_the_same_on_every_run() {
            let ids = |mailbox: crate::model::Mailbox| {
                let mut ids: Vec<_> = mailbox.messages.keys().copied().collect();
                ids.sort();
                ids
            };
            assert_eq!(
                ids(crate::sample::sample_mailbox()),
                ids(crate::sample::sample_mailbox())
            );
        }

        /// History previews a past version from the document alone: it reads
        /// no accounts and starts no engine, so the preview of a version is
        /// the onboarding over that version's state.
        #[test]
        fn a_past_version_reads_no_accounts() {
            ui::i18n::I18nManager::init("en-US");
            let app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let latest = app.versions().last().map(|version| version.hash).unwrap();
            let past = app.past_version(&[latest]).unwrap();
            let shown = past.texts().join("\n");
            assert!(shown.contains("No email account yet"), "{shown}");
            for account in crate::sample::sample_account_store().accounts {
                assert!(!shown.contains(&account.config.email), "{shown}");
            }
        }
    }

    mod draft_body {
        use tilo_document::RichTextAction;
        use ui::framework::testing::TestApp;
        use vello::kurbo::Rect;

        use super::super::MailShell;
        use crate::resource::TestMail;

        /// The draft body's paragraphs as stored.
        fn body(app: &TestApp<MailShell>) -> Vec<String> {
            app.read_document(|doc| {
                TestMail::draft_body(doc)
                    .and_then(|root| tilo_rich_text::blocks_under(doc, &root).ok())
                    .map(|blocks| blocks.into_iter().map(|block| block.text).collect())
                    .unwrap_or_default()
            })
        }

        /// The draft body as the HTML part it is sent as.
        fn html(app: &TestApp<MailShell>) -> String {
            app.read_document(|doc| {
                let root = TestMail::draft_body(doc).unwrap();
                tilo_doc_codecs::export_html::render_html_fragment_under(
                    doc,
                    &root,
                    &tilo_content::NoContent,
                )
                .unwrap_or_default()
            })
        }

        /// Where the composer shows its control `target` (`body`, `bold`).
        fn control(app: &mut TestApp<MailShell>, target: &str) -> Rect {
            fn find(node: &serde_json::Value, target: &str) -> Option<serde_json::Value> {
                match node {
                    serde_json::Value::Object(fields) => {
                        if fields.get("action_target").and_then(|t| t.as_str()) == Some(target) {
                            return fields.get("bounds").cloned();
                        }
                        fields.values().find_map(|value| find(value, target))
                    }
                    serde_json::Value::Array(items) => {
                        items.iter().find_map(|value| find(value, target))
                    }
                    _ => None,
                }
            }
            let inspection: serde_json::Value =
                serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
            let bounds = find(&inspection, &format!("mail.compose:{target}"))
                .unwrap_or_else(|| panic!("the composer shows {target}: {inspection}"));
            let edge = |name: &str| bounds[name].as_f64().unwrap();
            Rect::new(edge("x0"), edge("y0"), edge("x1"), edge("y1"))
        }

        /// The path of the widget that publishes the composer's control
        /// `target`, to address it the way assistive technology does.
        fn owner(app: &mut TestApp<MailShell>, target: &str) -> String {
            fn holds(node: &serde_json::Value, target: &str) -> bool {
                match node {
                    serde_json::Value::Object(fields) => {
                        fields.get("action_target").and_then(|t| t.as_str()) == Some(target)
                            || fields.values().any(|value| holds(value, target))
                    }
                    serde_json::Value::Array(items) => items.iter().any(|v| holds(v, target)),
                    _ => false,
                }
            }
            fn find(node: &serde_json::Value, target: &str) -> Option<String> {
                let children = node.get("children")?.as_array()?;
                if let Some(path) = children.iter().find_map(|child| find(child, target)) {
                    return Some(path);
                }
                node.get("semantics")
                    .is_some_and(|semantics| holds(semantics, target))
                    .then(|| node.get("path")?.as_str().map(str::to_owned))
                    .flatten()
            }
            let inspection: serde_json::Value =
                serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
            find(&inspection["root"], target)
                .unwrap_or_else(|| panic!("something publishes {target}: {inspection}"))
        }

        /// Mail with the composer open and the caret in its body.
        fn composing() -> TestApp<MailShell> {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::new().unwrap();
            app.call("compose", serde_json::json!({})).unwrap();
            let body = control(&mut app, "body");
            app.harness().click_at(body.x0 + 20.0, body.y0 + 16.0);
            app
        }

        /// The format a person writes in follows them: switching it is
        /// stored in their Mail preferences, and a window opened later (on
        /// this device or another) starts its compositions in it.
        #[test]
        fn the_compose_format_follows_the_person() {
            use super::super::MailPreferences;
            use crate::model::ComposeFormat;

            let mut app = composing();
            let format = control(&mut app, "format");
            app.harness().click_at(format.center().x, format.center().y);
            assert_eq!(
                app.settings::<MailPreferences>().get().compose_format,
                ComposeFormat::Text
            );

            let mut later = TestApp::<MailShell>::new().unwrap();
            later
                .settings::<MailPreferences>()
                .set(&MailPreferences {
                    compose_format: ComposeFormat::Text,
                    ..MailPreferences::default()
                })
                .unwrap();
            later.harness().relayout();
            later.call("compose", serde_json::json!({})).unwrap();
            let shown = later.harness().inspection_json().unwrap();
            assert!(shown.contains("mail.compose:body"), "the composer is open");
            assert!(
                !shown.contains("mail.compose:bold"),
                "a plain-text composition has no formatting: {shown}"
            );
        }

        #[test]
        fn typing_in_the_draft_body_edits_it_through_mail_a_word_a_step() {
            let mut app = composing();
            app.harness().type_text("Hi there");
            assert_eq!(body(&app), ["Hi there"]);
            let edits = app
                .history()
                .into_iter()
                .filter(|context| {
                    matches!(&context.what, Some(tilo_actions::Did::Edit(edit))
                        if edit.field == "rich_body")
                })
                .count();
            assert!(edits > 0, "history names the draft body as what was edited");
            assert!(app.undo());
            assert_eq!(body(&app), ["Hi "]);
        }

        #[test]
        fn formatting_marks_what_is_typed_next() {
            let mut app = composing();
            let bold = control(&mut app, "bold");
            app.harness().click_at(bold.center().x, bold.center().y);
            app.harness().type_text("Hi");
            assert_eq!(body(&app), ["Hi"]);
            let html = html(&app);
            assert!(html.contains("<strong>Hi</strong>"), "got: {html}");
        }

        #[test]
        fn the_link_prompt_links_the_typed_address() {
            let mut app = composing();
            // The command may sit in the bar's overflow menu in a narrow
            // composer, so it is addressed by its name, as a screen reader
            // does.
            let owner = owner(&mut app, "mail.compose:link");
            app.harness()
                .perform_semantic_named(
                    &owner,
                    Some("mail.compose:link"),
                    &ui::a11y::Action::Activate,
                )
                .unwrap();
            assert!(
                app.harness()
                    .inspection_json()
                    .unwrap()
                    .contains("mail.compose:prompt"),
                "the link prompt opened"
            );
            // The prompt starts with the scheme; the address goes after it.
            app.harness().key("End");
            app.harness().type_text("example.test/docs");
            app.harness().key("Enter");
            let html = html(&app);
            assert!(
                html.contains("href=\"https://example.test/docs\""),
                "got: {html}"
            );
        }

        #[test]
        fn an_agent_edits_the_draft_body_through_its_own_address() {
            let mut app = composing();
            app.call(
                "field-edit",
                serde_json::json!({
                    "edit": crate::resource::draft_body()
                        .some()
                        .perform(&RichTextAction::ReplaceBody {
                            markdown: "from an agent".into(),
                        })
                        .unwrap(),
                }),
            )
            .unwrap();
            assert_eq!(body(&app), ["from an agent"]);
            let shown = app.harness().inspection_json().unwrap();
            assert!(shown.contains("from an agent"), "the composer shows it");
            assert!(app.undo());
            assert_eq!(body(&app), [""]);
        }
    }
}
