//! The connector over native Mail's engine (`mail::engine`): IMAP and SMTP in native code, where
//! the network and the credentials are. It runs in the process the system starts for the app
//! ([`crate::serve`]), or in a window's own process outside a tilolang host.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{Receiver, Sender};

use mail::engine::{EngineEvent, EngineRequest, EngineRequestKind};
use mail::model::{AccountConfig, Draft, Folder, Mailbox, Message};
use mail::sync::{Want, Window};

use crate::connector::{
    AccountRef, Changes, Event, ImapMessage, MailConnector, Outgoing, Request, Unmade,
};

/// The key a message is known by.
#[must_use]
pub fn message_key(m: &Message) -> String {
    m.rfc_message_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| m.id.to_string())
}

/// A message of the engine as the connector shows it.
#[must_use]
pub fn imap_message(m: &Message) -> ImapMessage {
    let key = message_key(m);
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
        message_id: m.rfc_message_id.clone().unwrap_or_default(),
        references: m.in_reply_to.clone().unwrap_or_default(),
        attachments: m.attachments.iter().map(|a| a.filename.clone()).collect(),
        unread: !m.read,
        flagged: m.starred,
        mailbox: m.folder.as_key().to_owned(),
    }
}

/// Where the accounts come from: the settings' account store, or a test's.
pub type AccountSource = Box<dyn Fn() -> Vec<AccountConfig>>;

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
    keys: HashMap<(String, String), mail::model::MessageId>,
    /// Every request not answered yet: what it is, the account it is for, and for a move the
    /// mailbox the message went to, whose changes say where the server put it.
    pending: BTreeMap<Request, (EngineRequestKind, String, Option<String>)>,
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
        }
    }

    fn config(&self, account: &str) -> Option<AccountConfig> {
        (self.source)().into_iter().find(|c| c.email == account)
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

    fn held_message(&self, account: &str, key: &str) -> Option<Message> {
        let id = self.keys.get(&(account.to_owned(), key.to_owned()))?;
        self.held.get(account)?.get(*id).cloned()
    }

    fn take_in(&mut self, account: &str, folder: Folder, messages: &[Message]) -> Vec<ImapMessage> {
        let held = self.held.entry(account.to_owned()).or_default();
        let mut out = Vec::with_capacity(messages.len());
        for m in messages {
            let mut m = m.clone();
            if m.account.is_empty() {
                account.clone_into(&mut m.account);
            }
            m.folder = folder;
            self.keys
                .insert((account.to_owned(), message_key(&m)), m.id);
            out.push(imap_message(&m));
            held.insert(m);
        }
        out
    }
}

impl MailConnector for EngineConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        (self.source)()
            .into_iter()
            .map(|c| AccountRef {
                id: c.email.clone(),
                name: if c.display_name.is_empty() {
                    c.email.clone()
                } else {
                    c.display_name
                },
                address: c.email,
            })
            .collect()
    }

    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request> {
        let config = self.config(account)?;
        let folder = Folder::from_key(mailbox)?;
        let known = self
            .held
            .get(account)
            .and_then(|held| Window::of(held, account, folder));
        self.request(EngineRequest::fetch_wanting(
            config,
            folder,
            Want::Latest { known },
        ))
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

    fn search(&mut self, account: &str, text: &str) -> Option<Request> {
        let config = self.config(account)?;
        self.request(EngineRequest::search(config, text.to_owned()))
    }

    fn send(&mut self, account: &str, message: Outgoing) -> Result<Request, Unmade> {
        let config = self.config(account).ok_or(Unmade::NoAccount)?;
        let mut draft = Draft {
            to: message.to,
            cc: message.cc,
            subject: message.subject,
            body: message.body,
            ..Draft::default()
        };
        if !message.in_reply_to.is_empty() {
            draft.wire.in_reply_to = Some(message.in_reply_to);
        }
        draft.wire.references = message
            .references
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        self.request(EngineRequest::send(config, Box::new(draft)))
            .ok_or(Unmade::EngineStopped)
    }

    fn watch(&mut self, account: &str) {
        if let Some(config) = self.config(account) {
            // A watch is answered only by what arrives: one not made is said by `request`.
            let _ = self.request(EngineRequest::watch(config));
        }
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
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
        for (request, (kind, account, _)) in std::mem::take(&mut self.pending) {
            out.push(match kind {
                EngineRequestKind::Send => Event::Unsent {
                    request,
                    reason: WHY.to_owned(),
                },
                EngineRequestKind::StoreFlag | EngineRequestKind::Move => Event::Refused {
                    request,
                    reason: WHY.to_owned(),
                },
                _ => Event::Unreachable {
                    account,
                    reason: WHY.to_owned(),
                },
            });
        }
    }
}

impl EngineConnector {
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

    /// Take in what the engine said, adding what it means to `out`.
    pub fn engine_event(&mut self, event: EngineEvent, out: &mut Vec<Event>) {
        match event {
            EngineEvent::Fetched {
                account_id,
                folder,
                synced,
                ..
            } => {
                let messages = self.take_in(&account_id, folder, &synced.messages);
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
                }));
            }
            EngineEvent::Found {
                account_id,
                messages,
                ..
            } => {
                let found = messages.iter().map(imap_message).collect();
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
                let account = self
                    .pending
                    .remove(&request_id)
                    .map(|(_, account, _)| account)
                    .unwrap_or_default();
                match kind {
                    EngineRequestKind::Send if context == mail::engine::SMTP_UNREACHED => {
                        out.push(Event::Unsent {
                            request: request_id,
                            reason: message,
                        });
                    }
                    EngineRequestKind::Send
                    | EngineRequestKind::StoreFlag
                    | EngineRequestKind::Move => out.push(Event::Refused {
                        request: request_id,
                        reason: message,
                    }),
                    _ => out.push(Event::Unreachable {
                        account,
                        reason: message,
                    }),
                }
            }
            EngineEvent::Completed { request_id, kind } => {
                // A write's completion with no error before it: the server took it.
                let Some((_, account, to)) = self.pending.remove(&request_id) else {
                    return;
                };
                if matches!(
                    kind,
                    EngineRequestKind::Send
                        | EngineRequestKind::StoreFlag
                        | EngineRequestKind::Move
                ) {
                    out.push(Event::Accepted(request_id));
                }
                // A moved message has a new place on the server: its new mailbox's
                // changes say which.
                if let Some(to) = to {
                    self.ask_moved(&account, &to);
                }
            }
            EngineEvent::Connected { .. }
            | EngineEvent::Folders { .. }
            | EngineEvent::Sent { .. }
            | EngineEvent::Authorized { .. } => {}
        }
    }
}
