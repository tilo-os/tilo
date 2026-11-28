//! The connector over Mail's engine (`mail_engine::engine`): IMAP and SMTP in native code, where
//! the network and the credentials are. It runs in the process the system starts for the app
//! ([`crate::serve`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use mail_engine::attachment_store;
use mail_engine::engine::{EngineEvent, EngineRequest, EngineRequestKind};
use mail_engine::model::{
    AccountConfig, Draft, DraftAttachment, Folder, Mailbox, Message, SendFile,
};
use mail_engine::sync::{Want, Window};
use tilo_content::ContentStore;

use crate::connector::{
    AccountRef, Changes, Event, ImapMessage, MailConnector, Outgoing, Part, Request, Unmade,
};

/// The key a message is known by.
#[must_use]
pub fn message_key(m: &Message) -> String {
    m.rfc_message_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| m.id.to_string())
}

/// Where the OS installer keeps the mail it brought over from another mail program, a file an
/// account: `~/.cache/so.tilo.mail/mail`, as it wrote it there and as the engine's store reads
/// it.
fn kept_mail_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".cache")
            .join("so.tilo.mail")
            .join("mail"),
    )
}

/// What kind of trouble `reason` says: `no-password` for an account the engine signed in to
/// without one, `withheld` where the session kept the password back (`refused`), `signed-out`
/// for a login the server refused, and `unreachable` otherwise (a server that could not be
/// reached; one the device cannot reach for want of a network arrives as an unsent send, which
/// the origin says as `offline`).
#[must_use]
pub fn trouble_kind(reason: &str, withheld: bool) -> &'static str {
    if reason == mail_engine::engine::NO_PASSWORD {
        if withheld { "withheld" } else { "no-password" }
    } else if is_sign_in_refused(reason) {
        "signed-out"
    } else {
        "unreachable"
    }
}

/// Whether the server's words are a sign-in it refused, in the words the engine gives a person
/// ("IMAP server rejected the saved credential") or a protocol's own.
fn is_sign_in_refused(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("rejected the saved credential")
        || lower.contains("authentication failed")
        || lower.contains("authenticationfailed")
        || lower.contains("invalid credentials")
        || lower.contains("login failed")
        || lower.contains("535")
}

/// The message an `Outgoing` is, as the engine holds a draft to send or keep.
fn drafted(account: &str, message: Outgoing) -> Draft {
    let mut draft = Draft {
        to: message.to,
        cc: message.cc,
        bcc: message.bcc,
        subject: message.subject,
        body: message.body,
        ..Draft::default()
    };
    if !message.from.is_empty() {
        draft.sender = Some(mail_engine::model::DraftSender {
            account: account.to_owned(),
            email: message.from,
        });
    }
    if !message.html_body.is_empty() {
        draft.html_body = Some(message.html_body);
    }
    if !message.in_reply_to.is_empty() {
        draft.wire.in_reply_to = Some(message.in_reply_to);
    }
    draft.wire.references = message
        .references
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if !message.calendar_ics.is_empty() {
        draft.calendar_ics = Some(message.calendar_ics);
    }
    draft
}

/// A message of the engine as the connector shows it.
/// A message of the engine as the connector shows it, the files it carries kept in `content`:
/// a file whose bytes the message did not bring, or that no store keeps, is named with no content.
/// A picture its HTML shows in place (`cid:`) is no file of it: the HTML reads it at the address
/// of its content instead.
#[must_use]
pub fn imap_message(m: &Message, content: Option<&ContentStore>) -> ImapMessage {
    let key = message_key(m);
    let mut html = m.html_body.clone().unwrap_or_default();
    let mut attachments = Vec::with_capacity(m.attachments.len());
    for a in &m.attachments {
        let part = Part {
            name: a.filename.clone(),
            media: a.mime.clone(),
            size: a.size,
            content: kept(content, &a.content, &a.filename),
            inline_id: a.inline_id.clone().unwrap_or_default(),
        };
        if part.inline_id.is_empty() {
            attachments.push(part);
        } else if !part.content.is_empty() {
            html = in_place(&html, &part.inline_id, &part.content);
        }
    }
    let mut to = String::new();
    for a in &m.to {
        if !to.is_empty() {
            to.push_str(", ");
        }
        to.push_str(&a.email);
    }
    let mut cc = String::new();
    for a in &m.cc {
        if !cc.is_empty() {
            cc.push_str(", ");
        }
        cc.push_str(&a.email);
    }
    ImapMessage {
        thread: m.in_reply_to.clone().unwrap_or_else(|| key.clone()),
        key,
        subject: m.subject.clone(),
        from: m.from.email.clone(),
        from_name: m.from.display(),
        to,
        cc,
        received: u64::try_from(m.timestamp_min.max(0))
            .unwrap_or_default()
            .saturating_mul(60),
        body: m.body.clone(),
        html,
        message_id: m.rfc_message_id.clone().unwrap_or_default(),
        references: m.in_reply_to.clone().unwrap_or_default(),
        attachments,
        unread: !m.read,
        flagged: m.starred,
        mailbox: m.folder.as_key().to_owned(),
    }
}

/// `html` with each reference to the part `id` (`cid:<id>`, in any case) the address of its
/// content, `reference`.
fn in_place(html: &str, id: &str, reference: &str) -> String {
    let wanted = format!("cid:{}", id.to_ascii_lowercase());
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    // Lowercasing ASCII keeps every byte where it was, so a place in one is the same in both.
    let (mut rest, mut lower_rest) = (html, lower.as_str());
    while let Some(found) = lower_rest.find(&wanted) {
        let end = found.saturating_add(wanted.len());
        let (Some((before, _)), Some((_, after)), Some((_, lower_after))) = (
            rest.split_at_checked(found),
            rest.split_at_checked(end),
            lower_rest.split_at_checked(end),
        ) else {
            break;
        };
        out.push_str(before);
        out.push_str("tilo-blob://");
        out.push_str(reference);
        rest = after;
        lower_rest = lower_after;
    }
    out.push_str(rest);
    out
}

/// `bytes` kept in `content`: the reference they are kept under, or empty when there are none
/// or nowhere to keep them.
fn kept(content: Option<&ContentStore>, bytes: &[u8], name: &str) -> String {
    let Some(content) = content.filter(|_| !bytes.is_empty()) else {
        return String::new();
    };
    match content.put(bytes) {
        Ok(reference) => reference,
        Err(error) => {
            tracing::warn!(%error, name, "a file a message carries could not be kept");
            String::new()
        }
    }
}

/// The files `parts` names with the bytes `content` keeps of each, as a message being sent takes
/// them along; or which one is not on this device.
fn files_of(content: Option<&ContentStore>, parts: &[Part]) -> Result<Vec<SendFile>, String> {
    parts
        .iter()
        .map(|part| {
            let bytes = content
                .and_then(|content| content.read(&part.content))
                .ok_or_else(|| format!("{} is not on this device", part.name))?;
            Ok(SendFile {
                attachment: DraftAttachment {
                    content_id: format!("{}{}", attachment_store::PREFIX, part.content),
                    filename: attachment_store::safe_filename(&part.name),
                    mime: attachment_store::safe_mime(&part.media),
                    size: bytes.len() as u64,
                },
                bytes,
            })
        })
        .collect()
}

/// An account the connector signs in to: its settings, with the password the session released
/// for it, or the session's words for why it released none.
#[derive(Clone)]
pub struct MailAccount {
    pub config: AccountConfig,
    pub refused: Option<String>,
}

impl From<AccountConfig> for MailAccount {
    fn from(config: AccountConfig) -> Self {
        Self {
            config,
            refused: None,
        }
    }
}

/// Where the accounts come from: the device's dispatch ([`crate::sign_ins`]), or a test's.
pub type AccountSource = Box<dyn Fn() -> Vec<MailAccount>>;

/// The connector over native Mail's engine, which runs requests one at a time on a worker
/// thread and holds one connection per account.
pub struct EngineConnector {
    source: AccountSource,
    requests: Sender<EngineRequest>,
    /// The engine's answers, when this connector is the one reading them; else whoever reads
    /// them hands this connector its own ([`Self::engine_event`]).
    events: Option<Receiver<EngineEvent>>,
    /// What is held of each account, by address: the engine's mailbox, which knows each
    /// message's place on the server.
    held: HashMap<String, Mailbox>,
    /// The key of each held message, by account and key, to its engine id.
    keys: HashMap<(String, String), mail_engine::model::MessageId>,
    /// Every request not answered yet: what it is, the account it is for, and for a move the
    /// mailbox the message went to, whose changes say where the server put it.
    pending: BTreeMap<Request, (EngineRequestKind, String, Option<String>)>,
    /// Every fetch not answered yet, by request: the account and mailbox it reads. A fetch of a
    /// folder already under way takes in what the server holds now, so a second one asked for
    /// meanwhile is no fetch at all: it would read the folder again from the state last taken
    /// in, as one never read, and its mail would be no news.
    fetches: BTreeMap<Request, (String, String)>,
    /// The folders a fetch was asked for while one was under way: asked for again as the fetch
    /// answers, so mail that came meanwhile is read by the next one and not lost.
    missed: BTreeSet<(String, String)>,
    /// What is answered before the engine is asked: the mail another mail program kept on this
    /// computer, which the OS installer brought over.
    ready: Vec<Event>,
    /// The account's mailboxes whose kept mail was taken in already, so it is taken in once.
    kept_taken: HashSet<(String, String)>,
    /// Where the installer keeps the mail it brought over, as it wrote it there.
    kept_root: Option<PathBuf>,
    /// The content store the files messages carry are kept in, and those a send takes along are
    /// read from: this device's, which the app's window reads. `None` names files with no
    /// content.
    content: Option<ContentStore>,
}

impl EngineConnector {
    /// The connector reading its accounts from `source`, its engine answering on
    /// `requests`/`events`.
    #[must_use]
    pub fn new(
        source: AccountSource,
        requests: Sender<EngineRequest>,
        events: Receiver<EngineEvent>,
    ) -> Self {
        Self {
            source,
            requests,
            events: Some(events),
            held: HashMap::new(),
            keys: HashMap::new(),
            pending: BTreeMap::new(),
            fetches: BTreeMap::new(),
            missed: BTreeSet::new(),
            ready: Vec::new(),
            kept_taken: HashSet::new(),
            kept_root: kept_mail_root(),
            content: None,
        }
    }

    /// The connector reading its accounts from `source` and sending its requests to an engine
    /// whose answers someone else reads and hands it ([`Self::engine_event`]): one of several
    /// sharing an engine.
    #[must_use]
    pub fn sharing(source: AccountSource, requests: Sender<EngineRequest>) -> Self {
        Self {
            source,
            requests,
            events: None,
            held: HashMap::new(),
            keys: HashMap::new(),
            pending: BTreeMap::new(),
            fetches: BTreeMap::new(),
            missed: BTreeSet::new(),
            ready: Vec::new(),
            kept_taken: HashSet::new(),
            kept_root: kept_mail_root(),
            content: None,
        }
    }

    /// The connector taking its kept mail from `root` instead of the installer's own place:
    /// what a test of the import hands it.
    #[must_use]
    pub fn keeping(mut self, root: Option<PathBuf>) -> Self {
        self.kept_root = root;
        self
    }

    /// This connector keeping the files messages carry in `content`.
    #[must_use]
    pub fn keeping_files(mut self, content: Option<ContentStore>) -> Self {
        self.content = content;
        self
    }

    fn config(&self, account: &str) -> Option<AccountConfig> {
        (self.source)()
            .into_iter()
            .find(|signed| signed.config.email == account)
            .map(|signed| signed.config)
    }

    /// The engine's `message` for `account`, or, where it signed in to no server for want of a
    /// password, why the session released none, in its words.
    fn in_words(&self, account: &str, message: String) -> String {
        if message != mail_engine::engine::NO_PASSWORD {
            return message;
        }
        (self.source)()
            .into_iter()
            .find(|signed| signed.config.email == account)
            .and_then(|signed| signed.refused)
            .unwrap_or(message)
    }

    fn request(&mut self, request: EngineRequest) -> Option<Request> {
        self.request_to(request, None)
    }

    fn request_to(&mut self, request: EngineRequest, to: Option<String>) -> Option<Request> {
        let id = request.request_id();
        let kind = request.kind();
        let account = request.account().to_owned();
        if self.requests.send(request).is_err() {
            // The engine's worker is gone: what it would have answered is answered by
            // `events` as it finds the worker gone.
            tracing::error!(
                account,
                ?kind,
                "the mail engine stopped: a request was not made"
            );
            return None;
        }
        self.pending.insert(id, (kind, account, to));
        Some(id)
    }

    /// The mail `mailbox` of `account` holds of what the OS installer kept on this computer,
    /// taken in once: the messages an imported account shows before its sign-in is given, its
    /// server gone or never signed in to. None once they were taken, or for an account the
    /// settings say was not imported.
    fn take_in_kept(&mut self, account: &str, mailbox: &str) -> Option<Vec<ImapMessage>> {
        if !self
            .kept_taken
            .insert((account.to_owned(), mailbox.to_owned()))
        {
            return None;
        }
        if !self.config(account)?.import_pending {
            return None;
        }
        let folder = Folder::from_key(mailbox)?;
        let kept: Vec<Message> = mail_engine::store::Store::at(self.kept_root.clone()?)
            .load(account)
            .into_iter()
            .filter(|message| message.folder == folder)
            .collect();
        if kept.is_empty() {
            return None;
        }
        Some(self.take_in(account, folder, &kept))
    }

    fn held_message(&self, account: &str, key: &str) -> Option<Message> {
        let id = self.keys.get(&(account.to_owned(), key.to_owned()))?;
        self.held.get(account)?.get(*id).cloned()
    }

    /// The messages held of `folder` that the server no longer has: every one of them that the
    /// answer does not carry when it starts the folder over (a renumbered folder, or too much
    /// missed), else the ones inside the window the answer reports and does not name.
    fn gone_messages(
        &self,
        account: &str,
        folder: Folder,
        synced: &mail_engine::sync::Synced,
    ) -> (Vec<String>, Vec<mail_engine::model::MessageId>) {
        let Some(held) = self.held.get(account) else {
            return (Vec::new(), Vec::new());
        };
        let here = |m: &Message| {
            m.account == account
                && m.folder == folder
                && m.server.is_some_and(|at| at.folder == folder)
        };
        let mut gone: Vec<(String, mail_engine::model::MessageId)> = Vec::new();
        if synced.reset {
            let fresh: BTreeSet<String> = synced.messages.iter().map(message_key).collect();
            for m in held.messages.values().filter(|m| here(m)) {
                if !fresh.contains(&message_key(m)) {
                    gone.push((message_key(m), m.id));
                }
            }
        } else if let Some((oldest, newest)) = synced.window {
            let reported: BTreeSet<u32> = synced.flags.iter().map(|(uid, ..)| *uid).collect();
            for m in held.messages.values().filter(|m| here(m)) {
                let Some(at) = m.server else { continue };
                if at.uid >= oldest && at.uid <= newest && !reported.contains(&at.uid) {
                    gone.push((message_key(m), m.id));
                }
            }
        }
        let keys = gone.iter().map(|(key, _)| key.clone()).collect();
        let ids = gone.into_iter().map(|(_, id)| id).collect();
        (keys, ids)
    }

    /// Whether a fetch of the `account`'s `mailbox` is under way: one that answers what the
    /// server holds now makes a second, asked for meanwhile, no fetch at all.
    fn fetching(&self, account: &str, mailbox: &str) -> bool {
        self.fetches
            .values()
            .any(|(asked_of, folder)| asked_of == account && folder == mailbox)
    }

    /// What a fetch brought: its request answered, its messages taken in, and a folder asked for
    /// while it ran read now, with what it took in, so mail that came meanwhile is news.
    fn fetched(
        &mut self,
        request_id: Request,
        account: &str,
        folder: Folder,
        synced: &mail_engine::sync::Synced,
        out: &mut Vec<Event>,
    ) {
        self.fetches.remove(&request_id);
        self.took_fetched(account.to_owned(), folder, synced, out);
        if self
            .missed
            .remove(&(account.to_owned(), folder.as_key().to_owned()))
        {
            self.changes(account, folder.as_key());
        }
    }

    fn take_in(&mut self, account: &str, folder: Folder, messages: &[Message]) -> Vec<ImapMessage> {
        // The engine answers in the order it was asked: while a move of the account's is
        // unanswered, a fetch that answers before it read its folder before the move. A message
        // moved here and not yet seen in its new folder keeps where it was moved to; otherwise
        // a fetch of the folder it left puts it back there, and a purge of it asks that folder
        // for a message it no longer holds, leaving it on the server.
        let moving = self
            .pending
            .values()
            .any(|(kind, asked_of, _)| *kind == EngineRequestKind::Move && asked_of == account);
        let held = self.held.entry(account.to_owned()).or_default();
        let mut out = Vec::with_capacity(messages.len());
        for m in messages {
            let mut m = m.clone();
            if m.account.is_empty() {
                account.clone_into(&mut m.account);
            }
            m.folder = folder;
            out.push(imap_message(&m, self.content.as_ref()));
            let moved_away = held
                .get(m.id)
                .is_some_and(|at| at.server.is_none() && at.folder != folder);
            if moving && moved_away {
                continue;
            }
            self.keys
                .insert((account.to_owned(), message_key(&m)), m.id);
            held.insert(m);
        }
        out
    }
}

impl MailConnector for EngineConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        (self.source)()
            .into_iter()
            .map(|signed| signed.config)
            .map(|c| AccountRef {
                id: c.email.clone(),
                name: if c.display_name.is_empty() {
                    c.email.clone()
                } else {
                    c.display_name
                },
                address: c.email,
                identities: c.identities,
            })
            .collect()
    }

    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request> {
        if let Some(kept) = self.take_in_kept(account, mailbox) {
            // Answered as the server's own answer is, without asking a server: the account's
            // mail is here, and the account says it needs its sign-in before a server can be
            // asked again.
            self.ready.push(Event::Changes(Changes {
                account: account.to_owned(),
                mailbox: mailbox.to_owned(),
                messages: kept,
                ..Changes::default()
            }));
            return Some(uuid::Uuid::new_v4());
        }
        let config = self.config(account)?;
        let folder = Folder::from_key(mailbox)?;
        if self.fetching(account, mailbox) {
            self.missed.insert((account.to_owned(), mailbox.to_owned()));
            return None;
        }
        let known = self
            .held
            .get(account)
            .and_then(|held| Window::of(held, account, folder));
        let request = self.request(EngineRequest::fetch_wanting(
            config,
            folder,
            Want::Latest { known },
        ));
        if let Some(id) = request {
            self.fetches
                .insert(id, (account.to_owned(), mailbox.to_owned()));
        }
        request
    }

    fn older(&mut self, account: &str, mailbox: &str) -> Option<Request> {
        let config = self.config(account)?;
        let folder = Folder::from_key(mailbox)?;
        if self.fetching(account, mailbox) {
            self.missed.insert((account.to_owned(), mailbox.to_owned()));
            return None;
        }
        let known = self
            .held
            .get(account)
            .and_then(|held| Window::of(held, account, folder))?;
        let request = self.request(EngineRequest::fetch_wanting(
            config,
            folder,
            Want::Older { known },
        ));
        if let Some(id) = request {
            self.fetches
                .insert(id, (account.to_owned(), mailbox.to_owned()));
        }
        request
    }

    fn flag(&mut self, account: &str, key: &str, unread: bool, flagged: bool) -> Option<Request> {
        let config = self.config(account)?;
        let mut m = self.held_message(account, key)?;
        m.read = !unread;
        m.starred = flagged;
        if let (Some(held), Some(id)) = (
            self.held.get_mut(account),
            self.keys.get(&(account.to_owned(), key.to_owned())),
        ) && let Some(at) = held.get_mut(*id)
        {
            at.read = m.read;
            at.starred = m.starred;
        }
        self.request(EngineRequest::store_flag(config, Box::new(m)))
    }

    fn move_to(&mut self, account: &str, key: &str, mailbox: &str) -> Option<Request> {
        let config = self.config(account)?;
        let to = Folder::from_key(mailbox)?;
        let m = self.held_message(account, key)?;
        if m.folder == to {
            return None;
        }
        if let (Some(held), Some(id)) = (
            self.held.get_mut(account),
            self.keys.get(&(account.to_owned(), key.to_owned())),
        ) && let Some(at) = held.get_mut(*id)
        {
            // Where the server puts it is not known until its new mailbox's changes say: until
            // then the message is found there by its Message-ID.
            at.folder = to;
            at.server = None;
        }
        self.request_to(
            EngineRequest::move_message(config, Box::new(m), to),
            Some(mailbox.to_owned()),
        )
    }

    fn purge(&mut self, account: &str, key: &str) -> Option<Request> {
        let config = self.config(account)?;
        let mut m = self.held_message(account, key)?;
        // Only what the trash holds is deleted for good: a message held as read elsewhere is
        // found in the trash by its Message-ID, never asked of a folder it has left.
        if m.server.is_none_or(|at| at.folder != Folder::Trash) {
            m.folder = Folder::Trash;
            m.server = None;
        }
        // It leaves the workspace's mail as it leaves the server: what is held here no longer
        // names it, so no flag or move a person's next change asks for is sent for it.
        if let Some(id) = self.keys.remove(&(account.to_owned(), key.to_owned()))
            && let Some(held) = self.held.get_mut(account)
        {
            held.purge(id);
        }
        self.request(EngineRequest::purge(config, Box::new(m)))
    }

    fn search(&mut self, account: &str, text: &str) -> Option<Request> {
        let config = self.config(account)?;
        self.request(EngineRequest::search(config, text.to_owned()))
    }

    fn save_draft(&mut self, account: &str, draft: Outgoing) -> Option<Request> {
        let config = self.config(account)?;
        self.request(EngineRequest::save_draft(
            config,
            Box::new(drafted(account, draft)),
        ))
    }

    fn send(&mut self, account: &str, message: Outgoing) -> Result<Request, Unmade> {
        let config = self.config(account).ok_or(Unmade::NoAccount)?;
        let files = files_of(self.content.as_ref(), &message.files).map_err(Unmade::Missing)?;
        let mut draft = Draft {
            to: message.to,
            cc: message.cc,
            bcc: message.bcc,
            subject: message.subject,
            body: message.body,
            ..Draft::default()
        };
        if !message.from.is_empty() {
            draft.sender = Some(mail_engine::model::DraftSender {
                account: account.to_owned(),
                email: message.from,
            });
        }
        if !message.html_body.is_empty() {
            draft.html_body = Some(message.html_body);
        }
        if !message.in_reply_to.is_empty() {
            draft.wire.in_reply_to = Some(message.in_reply_to);
        }
        draft.wire.references = message
            .references
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !message.calendar_ics.is_empty() {
            draft.calendar_ics = Some(message.calendar_ics);
        }
        self.request(EngineRequest::send(config, Box::new(draft), files))
            .ok_or(Unmade::EngineStopped)
    }

    fn watch(&mut self, account: &str) {
        if let Some(config) = self.config(account) {
            // A watch is answered only by what arrives: one not made is said by `request`.
            let _ = self.request(EngineRequest::watch(config));
        }
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = self.take_ready();
        while let Some(event) = self.next_event(&mut out) {
            self.engine_event(event, &mut out);
        }
        out
    }
}

/// Why what the engine held is answered unmade, once its worker ended.
const WHY: &str = "the mail engine stopped";

impl EngineConnector {
    /// The engine's next answer, for a connector that reads them; `None` once none waits, or
    /// the worker ended, which answers every request it held into `out`.
    fn next_event(&mut self, out: &mut Vec<Event>) -> Option<EngineEvent> {
        match self.events.as_ref()?.try_recv() {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.engine_stopped(out);
                None
            }
        }
    }

    /// Ask for the changes of `mailbox`, where a message of `account` was moved: they say its
    /// new place on the server.
    fn ask_moved(&mut self, account: &str, mailbox: &str) {
        if self.changes(account, mailbox).is_none() {
            tracing::warn!(
                account,
                mailbox,
                "a moved message's new place was not asked for"
            );
        }
    }

    /// The engine's worker ended: every request it did not answer is answered here, so nothing
    /// waits on it. A send it never made waits to go online (`Unsent`); a write is refused, so
    /// what it changed is put back; a read leaves its account unreachable.
    pub fn engine_stopped(&mut self, out: &mut Vec<Event>) {
        if self.pending.is_empty() {
            return;
        }
        tracing::error!(
            requests = self.pending.len(),
            "{WHY}: its requests are answered unsent"
        );
        self.fetches.clear();
        self.missed.clear();
        for (request, (kind, account, _)) in std::mem::take(&mut self.pending) {
            out.push(match kind {
                EngineRequestKind::Send => Event::Unsent {
                    request,
                    reason: WHY.to_owned(),
                },
                EngineRequestKind::StoreFlag
                | EngineRequestKind::Move
                | EngineRequestKind::Purge
                | EngineRequestKind::SaveDraft => Event::Refused {
                    request,
                    reason: WHY.to_owned(),
                },
                _ => Event::Unreachable {
                    account,
                    reason: WHY.to_owned(),
                    kind: "unreachable".to_owned(),
                },
            });
        }
    }
}

impl EngineConnector {
    /// What the connector answered without asking the engine — the mail the installer kept on
    /// this computer — taken so it is said once.
    pub fn take_ready(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.ready)
    }

    /// Whether the engine still works on a request of this connector's.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether the engine's request `request` is one this connector made and is waiting on.
    #[must_use]
    pub fn owns(&self, request: &Request) -> bool {
        self.pending.contains_key(request)
    }

    /// What a fetch of a mailbox brought: the messages taken in, the flags it reported of the
    /// messages held, and the messages the server no longer has; added to `out` as its changes.
    fn took_fetched(
        &mut self,
        account_id: String,
        folder: Folder,
        synced: &mail_engine::sync::Synced,
        out: &mut Vec<Event>,
    ) {
        // What the server no longer has, found before what it brought is taken in.
        let (removed, gone) = self.gone_messages(&account_id, folder, synced);
        let messages = self.take_in(&account_id, folder, &synced.messages);
        if let Some(held) = self.held.get_mut(&account_id) {
            for id in gone {
                held.remove(id);
            }
        }
        for key in &removed {
            self.keys.remove(&(account_id.clone(), key.clone()));
        }
        let mut flags = Vec::new();
        if let Some(held) = self.held.get_mut(&account_id) {
            for (uid, seen, flagged) in &synced.flags {
                let found = held.messages.values_mut().find(|m| {
                    m.server
                        .is_some_and(|at| at.folder == folder && at.uid == *uid)
                });
                if let Some(m) = found {
                    m.read = *seen;
                    m.starred = *flagged;
                    flags.push((message_key(m), !*seen, *flagged));
                }
            }
        }
        out.push(Event::Changes(Changes {
            account: account_id,
            mailbox: folder.as_key().to_owned(),
            messages,
            flags,
            removed,
            more: synced.has_older,
            reset: synced.reset,
        }));
    }

    /// Take in what the engine said, adding what it means to `out`.
    pub fn engine_event(&mut self, event: EngineEvent, out: &mut Vec<Event>) {
        match event {
            EngineEvent::Fetched {
                request_id,
                account_id,
                folder,
                synced,
                ..
            } => self.fetched(request_id, &account_id, folder, &synced, out),
            EngineEvent::Found {
                account_id,
                messages,
                ..
            } => {
                let found = messages
                    .iter()
                    .map(|m| imap_message(m, self.content.as_ref()))
                    .collect();
                out.push(Event::Found {
                    account: account_id,
                    messages: found,
                });
            }
            EngineEvent::Arrived { account_id, .. } => {
                out.push(Event::Arrived {
                    account: account_id,
                });
            }
            EngineEvent::Error {
                request_id,
                kind,
                context,
                message,
            } => {
                self.fetches.remove(&request_id);
                let account = self
                    .pending
                    .remove(&request_id)
                    .map(|(_, account, _)| account)
                    .unwrap_or_default();
                let withheld = (self.source)()
                    .into_iter()
                    .find(|signed| signed.config.email == account)
                    .is_some_and(|signed| signed.refused.is_some());
                let trouble = trouble_kind(&message, withheld);
                let message = self.in_words(&account, message);
                match kind {
                    EngineRequestKind::Send if context == mail_engine::engine::SMTP_UNREACHED => {
                        out.push(Event::Unsent {
                            request: request_id,
                            reason: message,
                        });
                    }
                    EngineRequestKind::Send
                    | EngineRequestKind::StoreFlag
                    | EngineRequestKind::Move
                    | EngineRequestKind::Purge
                    | EngineRequestKind::SaveDraft => out.push(Event::Refused {
                        request: request_id,
                        reason: message,
                    }),
                    _ => out.push(Event::Unreachable {
                        account,
                        reason: message,
                        kind: trouble.to_owned(),
                    }),
                }
            }
            EngineEvent::Completed { request_id, kind } => {
                // A write's completion with no error before it: the server took it.
                self.fetches.remove(&request_id);
                let Some((_, account, to)) = self.pending.remove(&request_id) else {
                    return;
                };
                if matches!(
                    kind,
                    EngineRequestKind::Send
                        | EngineRequestKind::StoreFlag
                        | EngineRequestKind::Move
                        | EngineRequestKind::Purge
                ) {
                    out.push(Event::Accepted(request_id));
                }
                // A moved message has a new place on the server: its new mailbox's
                // changes say which.
                if let Some(to) = to {
                    self.ask_moved(&account, &to);
                }
            }
            // A copy the account's server kept none of changes nothing this workspace holds:
            // the message left, which its completion says, and why its Sent folder holds none
            // of it goes to whoever waits on the send.
            EngineEvent::Unfiled { request_id, reason } => {
                out.push(Event::Unfiled {
                    request: request_id,
                    reason,
                });
            }
            EngineEvent::Connected { .. }
            | EngineEvent::Folders { .. }
            | EngineEvent::Sent { .. } => {}
        }
    }
}
