//! The IMAP origin: keeps a package's types of the `imap` kinds (`type Message (origin:
//! imap.message)`) in step with the servers of the workspace's mail accounts, through the
//! connector (`super::connector`), and carries out `smtp.send` and `imap.check`.
//!
//! - What a server holds is written into the package's records as changes of the origin: an
//!   account's mailboxes, its conversations and their messages, keyed by the server's ids so
//!   nothing appears twice.
//! - What people change in the fields people change (a message's `unread`, `flagged` and
//!   `mailbox`) applies at once in the record and is handed to the server; a refusal puts the
//!   field back and says why on the account.
//! - A send waits in the outbox until the server took it; while it has not, the outbox keeps it
//!   with the reason.
//! - A person asking for their mail again (`imap.check`) asks the account's server now, and the
//!   account's own fields say how it answered.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::runtime::RuntimeError;
use tilolang_tilo::runtime::origin::{OriginChange, OriginWrite, Unwritten};
use tilolang_tilo::runtime::store::{FieldKey, Store as _};
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use super::connector::{Changes, Event, ImapMessage, MailConnector, Outgoing, Request, Unmade};

/// The mailboxes every account shows, in order: key, name, role.
pub const MAILBOXES: &[(&str, &str)] = &[
    ("inbox", "Inbox"),
    ("archive", "Archive"),
    ("sent", "Sent"),
    ("drafts", "Drafts"),
    ("junk", "Junk"),
    ("trash", "Trash"),
];

/// The mailboxes whose changes are asked for when an account is first seen.
const SYNCED: &[&str] = &["inbox", "archive", "sent", "trash"];

/// What people change of a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct People {
    unread: bool,
    flagged: bool,
    mailbox: String,
}

struct Msg {
    record: ResourceRef,
    thread: String,
    received: u64,
    from_name: String,
    snippet: String,
    /// What the server holds, as far as it was told.
    confirmed: People,
}

struct Thr {
    record: ResourceRef,
    keys: Vec<String>,
}

struct Acct {
    record: ResourceRef,
    boxes: HashMap<String, ResourceRef>,
    threads: HashMap<String, Thr>,
    messages: HashMap<String, Msg>,
}

#[derive(Clone)]
struct Types {
    /// The root's collection of accounts.
    accounts_field: String,
    account: OriginType,
    mailbox: OriginType,
    thread: OriginType,
    message: OriginType,
}

struct State {
    types: Types,
    root: ResourceRef,
    accounts: HashMap<String, Acct>,
    /// The account and key of each message record, by its id.
    by_record: HashMap<String, (String, String)>,
}

/// A request handed to a server and not answered yet.
enum Pending {
    Send(String),
    Write {
        account: String,
        key: String,
        before: People,
    },
}

enum Sending {
    Going,
    Gone,
    Refused(String),
    /// The server could not be reached: the message waits to go online.
    Unsent(String),
}

/// A step of the sync the workspace could not take, kept to be written again by the next poll:
/// what a server said is not lost with the poll that could not keep it.
enum Step {
    /// What a server said changed in an account.
    In(Changes),
    /// A message's fields put back as the server holds them, after the server refused them.
    Back {
        account: String,
        key: String,
        before: People,
    },
    /// The summaries of the conversations a person's change touched.
    Summaries {
        account: String,
        threads: BTreeSet<String>,
    },
}

impl Step {
    fn account(&self) -> &str {
        match self {
            Self::In(changes) => &changes.account,
            Self::Back { account, .. } | Self::Summaries { account, .. } => account,
        }
    }
}

/// What the workspace made of one step of the sync.
enum Taken {
    /// It was written.
    Wrote,
    /// There was nothing left to write: the step is done with.
    Nothing,
    /// The workspace could not take it: it waits for the next poll, and the account says why.
    Refused,
}

impl Taken {
    /// Whether it is done with, waiting for no poll.
    fn done(&self) -> bool {
        !matches!(self, Self::Refused)
    }

    /// Whether the step wrote: a refused one leaves the account's reason to be written too.
    fn wrote(&self) -> bool {
        !matches!(self, Self::Nothing)
    }
}

/// The origin. See the module docs.
pub struct ImapOrigin {
    connector: Box<dyn MailConnector>,
    /// Set when the connector has an answer, from the thread it answers on.
    woken: Arc<AtomicBool>,
    state: Option<State>,
    /// What changed in the room since the last poll, as the store told it.
    changed: Rc<RefCell<Vec<FieldKey>>>,
    pending: BTreeMap<Request, Pending>,
    sends: HashMap<String, Sending>,
    /// Why each account's server could not be reached, until it can.
    trouble: HashMap<String, String>,
    /// Why each account's mail could not be written into the workspace, until it can be.
    unsaved: HashMap<String, String>,
    /// When each account's mail was last taken in from its server: what the account says of
    /// itself, so a window can say how old the mail it shows is.
    fetched: HashMap<String, String>,
    /// The steps the workspace could not take, oldest first.
    unwritten: Unwritten<Step>,
    /// Accounts whose changes to take in, with the mailboxes to ask for.
    due: Vec<(String, String)>,
    /// Set when the device came online again: servers that could not be reached are asked
    /// again, and messages waiting to go online are sent.
    reconnected: Arc<AtomicBool>,
}

impl ImapOrigin {
    /// The origin over `connector`, which sets `woken` when it has an answer.
    #[must_use]
    pub fn new(connector: Box<dyn MailConnector>, woken: Arc<AtomicBool>) -> Self {
        Self {
            connector,
            woken,
            state: None,
            changed: Rc::default(),
            pending: BTreeMap::new(),
            sends: HashMap::new(),
            trouble: HashMap::new(),
            unsaved: HashMap::new(),
            fetched: HashMap::new(),
            unwritten: Unwritten::new(),
            due: Vec::new(),
            reconnected: Arc::default(),
        }
    }

    /// Online again, when it is: each server that could not be reached is asked what changed,
    /// and what waits to go online is sent. Whether the outbox is to be settled.
    fn reconnect(&mut self, host: &Host) -> bool {
        if !self.reconnected.swap(false, Ordering::AcqRel) {
            return false;
        }
        for account in self.trouble.keys() {
            self.due.push((account.clone(), "inbox".to_owned()));
        }
        host.runtime().retry_waiting();
        true
    }

    /// The account's server asked now what changed, as a person asking for their mail again
    /// does: its inbox is read again, and the account says how the server stands once it
    /// answers, as it does when the device comes online.
    fn check_now(&mut self, account: &str) -> Delivery {
        let known = self
            .state
            .as_ref()
            .is_some_and(|state| state.accounts.contains_key(account));
        if !known {
            return Delivery::Rejected(format!("no mail account {account} on this device"));
        }
        self.due.push((account.to_owned(), "inbox".to_owned()));
        // The ask is taken: what the server says of itself arrives as its mail does, on the
        // poll it wakes, and the account's own fields say how it stands.
        Delivery::Delivered
    }

    /// What the connector reported since the last frame: what changed, what
    /// became unreachable and how what was asked of it ended.
    fn take_events(&mut self, host: &Host, now: &str) -> Polled {
        let mut polled = Polled::default();
        for event in self.connector.events() {
            match event {
                Event::Changes(changes) => {
                    // The server answered with its mail: this is when the account's mail was
                    // last read from it, kept or not.
                    self.fetched.insert(changes.account.clone(), now.to_owned());
                    self.trouble.remove(&changes.account);
                    polled.wrote |= self.take_changes(host, now, changes).wrote();
                }
                Event::Found { account, messages } => {
                    self.fetched.insert(account.clone(), now.to_owned());
                    let changes = Changes {
                        account,
                        mailbox: String::new(),
                        messages,
                        flags: Vec::new(),
                    };
                    polled.wrote |= self.take_changes(host, now, changes).wrote();
                }
                Event::Arrived { account } => {
                    if self.connector.changes(&account, "inbox").is_none() {
                        tracing::warn!(account, "new mail arrived and was not asked for");
                    }
                }
                Event::Unreachable { account, reason } => {
                    self.trouble.insert(account, reason);
                }
                Event::Accepted(request) => match self.pending.remove(&request) {
                    Some(Pending::Send(payload)) => {
                        self.sends.insert(payload, Sending::Gone);
                        polled.settle = true;
                    }
                    Some(Pending::Write { .. }) | None => {}
                },
                Event::Refused { request, reason } => match self.pending.remove(&request) {
                    Some(Pending::Send(payload)) => {
                        self.sends.insert(payload, Sending::Refused(reason));
                        polled.settle = true;
                    }
                    Some(Pending::Write {
                        account,
                        key,
                        before,
                    }) => {
                        self.trouble.insert(account.clone(), reason);
                        polled.wrote |= self.write_back(host, now, account, key, before);
                    }
                    None => {}
                },
                Event::Unsent { request, reason } => match self.pending.remove(&request) {
                    Some(Pending::Send(payload)) => {
                        self.sends.insert(payload, Sending::Unsent(reason));
                        polled.settle = true;
                    }
                    Some(Pending::Write {
                        account,
                        key,
                        before,
                    }) => {
                        self.trouble.insert(account.clone(), reason);
                        polled.wrote |= self.write_back(host, now, account, key, before);
                    }
                    None => {}
                },
            }
        }
        polled
    }

    /// This origin, trying again what could not reach its server whenever `reconnected` is set
    /// (the device came online again) and it is woken.
    #[must_use]
    pub fn reconnecting_on(mut self, reconnected: Arc<AtomicBool>) -> Self {
        self.reconnected = reconnected;
        self
    }
}

fn text(s: &str) -> Value {
    Value::text(s)
}

fn put(ty: &OriginType, fields: &mut Vec<(String, Value)>, name: &str, value: Value) {
    if let Some(id) = ty.field(name) {
        fields.push((id.to_owned(), value));
    }
}

fn read(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> Option<Value> {
    host.runtime().read(at, ty.field(name)?).ok()
}

fn read_text(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> String {
    match read(host, at, ty, name) {
        Some(Value::Text(t)) => t,
        _ => String::new(),
    }
}

fn read_bool(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> bool {
    match read(host, at, ty, name) {
        Some(Value::Bool(b)) => b,
        Some(Value::Int(i)) => i != 0,
        _ => false,
    }
}

fn items(host: &Host, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match host.runtime().read(at, field) {
        Ok(Value::List(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::Ref(r) => Some((*r.target).clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn snippet_of(body: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(140).collect()
}

fn types_of(host: &Host, app: &str) -> Option<(Types, String)> {
    let package = host.package(app)?;
    let found = origin_types(&package.meta, "imap");
    let kind = |k: &str| found.iter().find(|t| t.kind == k).cloned();
    let account = kind("imap.account")?;
    let (_, root_ty) = package.main()?;
    let account_name = package
        .meta
        .types
        .iter()
        .find(|t| t.id == account.ty)?
        .name
        .clone();
    let root = package.meta.types.iter().find(|t| t.id == root_ty)?;
    let want = format!("Collection<{account_name}>");
    let accounts_field = root.fields.iter().find(|f| f.ty == want)?.id.clone();
    Some((
        Types {
            accounts_field,
            account,
            mailbox: kind("imap.mailbox")?,
            thread: kind("imap.thread")?,
            message: kind("imap.message")?,
        },
        root_ty,
    ))
}

impl ImapOrigin {
    /// The records the room holds already, read once: what was taken in before stays as it is.
    fn load(host: &Host, types: Types, root: ResourceRef) -> State {
        let mut state = State {
            types,
            root,
            accounts: HashMap::new(),
            by_record: HashMap::new(),
        };
        let t = &state.types;
        for record in items(host, &state.root, &t.accounts_field) {
            let key = read_text(host, &record, &t.account, "key");
            let mut acct = Acct {
                record: record.clone(),
                boxes: HashMap::new(),
                threads: HashMap::new(),
                messages: HashMap::new(),
            };
            if let Some(field) = t.account.field("mailboxes") {
                for b in items(host, &record, field) {
                    acct.boxes.insert(read_text(host, &b, &t.mailbox, "key"), b);
                }
            }
            if let Some(field) = t.account.field("threads") {
                for th in items(host, &record, field) {
                    let tkey = read_text(host, &th, &t.thread, "key");
                    let mut keys = Vec::new();
                    if let Some(mfield) = t.thread.field("messages") {
                        for m in items(host, &th, mfield) {
                            let mkey = read_text(host, &m, &t.message, "key");
                            let confirmed = People {
                                unread: read_bool(host, &m, &t.message, "unread"),
                                flagged: read_bool(host, &m, &t.message, "flagged"),
                                mailbox: read_text(host, &m, &t.message, "mailbox"),
                            };
                            let received = match read(host, &m, &t.message, "received") {
                                Some(Value::Text(at)) => seconds_of(&at),
                                _ => 0,
                            };
                            state
                                .by_record
                                .insert(m.id_string(), (key.clone(), mkey.clone()));
                            acct.messages.insert(
                                mkey.clone(),
                                Msg {
                                    record: m.clone(),
                                    thread: tkey.clone(),
                                    received,
                                    from_name: read_text(host, &m, &t.message, "from_name"),
                                    snippet: read_text(host, &m, &t.message, "snippet"),
                                    confirmed,
                                },
                            );
                            keys.push(mkey);
                        }
                    }
                    acct.threads.insert(tkey, Thr { record: th, keys });
                }
            }
            state.accounts.insert(key, acct);
        }
        state
    }

    /// Each account of the settings has its record and its mailboxes, made as one change of the
    /// origin, and its servers are asked what they hold. A change the workspace cannot take
    /// leaves no account without its mailboxes behind: the next poll makes it again.
    fn add_accounts(&mut self, host: &Host, now: &str) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let t = &state.types;
        let mut writes = Vec::new();
        let mut new_accounts = Vec::new();
        for a in self.connector.accounts() {
            if state.accounts.contains_key(&a.id) {
                continue;
            }
            let mut fields = Vec::new();
            put(&t.account, &mut fields, "key", text(&a.id));
            put(&t.account, &mut fields, "address", text(&a.address));
            put(&t.account, &mut fields, "name", text(&a.name));
            put(&t.account, &mut fields, "online", Value::Bool(false));
            writes.push(OriginWrite::Insert {
                owner: state.root.clone(),
                field: t.accounts_field.clone(),
                ty: t.account.ty.clone(),
                fields,
            });
            new_accounts.push(a.id);
        }
        if writes.is_empty() {
            return false;
        }
        let made = host
            .runtime()
            .write_origin("imap", Some(now.to_owned()), |change| {
                let records = change.write(writes)?;
                let mut accounts: Vec<(String, Acct)> = Vec::new();
                for (id, record) in new_accounts.into_iter().zip(records) {
                    let Some(record) = record else { continue };
                    let mut boxes = Vec::new();
                    if let Some(field) = t.account.field("mailboxes") {
                        let mut box_writes = Vec::new();
                        let mut keys = Vec::new();
                        for (key, name) in MAILBOXES {
                            let mut fields = Vec::new();
                            put(&t.mailbox, &mut fields, "key", text(key));
                            put(&t.mailbox, &mut fields, "name", text(name));
                            put(&t.mailbox, &mut fields, "role", text(key));
                            box_writes.push(OriginWrite::Insert {
                                owner: record.clone(),
                                field: field.to_owned(),
                                ty: t.mailbox.ty.clone(),
                                fields,
                            });
                            keys.push((*key).to_owned());
                        }
                        let records = change.write(box_writes)?;
                        for (key, record) in keys.into_iter().zip(records) {
                            if let Some(record) = record {
                                boxes.push((key, record));
                            }
                        }
                    }
                    accounts.push((
                        id,
                        Acct {
                            record,
                            boxes: boxes.into_iter().collect(),
                            threads: HashMap::new(),
                            messages: HashMap::new(),
                        },
                    ));
                }
                Ok(accounts)
            });
        let made = match made {
            Ok(made) => made,
            Err(error) => {
                // There is no account's record to say it on: it is said here, and the next poll
                // makes the accounts again.
                tracing::warn!(%error, "the mail accounts could not be written");
                return false;
            }
        };
        let mut new = Vec::new();
        for (id, acct) in made {
            state.accounts.insert(id.clone(), acct);
            new.push(id);
        }
        for id in new {
            for mailbox in SYNCED {
                self.due.push((id.clone(), (*mailbox).to_owned()));
            }
            self.connector.watch(&id);
        }
        true
    }

    /// What a server said changed, written into the account's conversations. A step the
    /// workspace cannot take leaves nothing of itself behind and gives why: the account is read
    /// again as the store holds it, so no message of the step is known, and the next poll takes
    /// the step in again whole.
    fn take_in(&mut self, host: &Host, now: &str, changes: &Changes) -> Result<bool, RuntimeError> {
        let Some(state) = self.state.as_mut() else {
            return Ok(false);
        };
        let (types, root) = (state.types.clone(), state.root.clone());
        match take_in_step(host, now, state, changes) {
            Ok(wrote) => Ok(wrote),
            Err(error) => {
                *state = Self::load(host, types, root);
                Err(error)
            }
        }
    }

    /// What a server said, taken in, or kept for the next poll with the account saying why it
    /// could not be: a step is never dropped because the workspace would not take it.
    fn take_step(&mut self, host: &Host, now: &str, changes: &Changes) -> Taken {
        match self.take_in(host, now, changes) {
            Ok(wrote) => {
                self.settled(&changes.account);
                if wrote { Taken::Wrote } else { Taken::Nothing }
            }
            Err(error) => {
                tracing::warn!(account = %changes.account, %error, "what a mail server said could not be written");
                self.unsaved.insert(
                    changes.account.clone(),
                    format!("The mail could not be saved: {error}"),
                );
                Taken::Refused
            }
        }
    }

    /// What a server said changed, written if the workspace takes it, and kept — behind what the
    /// server said before — if it does not.
    fn take_changes(&mut self, host: &Host, now: &str, changes: Changes) -> Taken {
        let taken = self.take_step(host, now, &changes);
        if !taken.done() {
            self.keep(changes);
        }
        taken
    }

    /// Keep what a server said for the next poll: one step waits per account, holding what the
    /// server said before it too, so the mail is taken in in the order the server gave it.
    fn keep(&mut self, changes: Changes) {
        let account = changes.account.clone();
        let Some(Step::In(held)) = self
            .unwritten
            .waiting_mut(|step| matches!(step, Step::In(held) if held.account == account))
        else {
            self.unwritten.keep(Step::In(changes));
            return;
        };
        for message in changes.messages {
            if !held.messages.iter().any(|known| known.key == message.key) {
                held.messages.push(message);
            }
        }
        for flag in changes.flags {
            if !held.flags.iter().any(|known| known.0 == flag.0) {
                held.flags.push(flag);
            }
        }
    }

    /// The account's mail is written: while no step of it waits, the account says nothing.
    fn settled(&mut self, account: &str) {
        if !self.unwritten.waits(|step| step.account() == account) {
            self.unsaved.remove(account);
        }
    }

    /// The steps the workspace could not take, taken again: what is written no longer waits, and
    /// what is not stays for the next poll, its account saying why.
    fn take_again(&mut self, host: &Host, now: &str) -> bool {
        let mut wrote = false;
        let mut unwritten = std::mem::take(&mut self.unwritten);
        unwritten.take_again(|step| {
            let taken = self.write_step(host, now, step);
            wrote |= taken.wrote();
            taken.done()
        });
        self.unwritten = unwritten;
        wrote
    }

    /// One step of the sync, written as one change of the origin: a step the workspace does not
    /// take leaves nothing of itself behind, and says on its account why the mail is not saved.
    fn write_step(&mut self, host: &Host, now: &str, step: &mut Step) -> Taken {
        match step {
            Step::In(changes) => self.take_step(host, now, changes),
            Step::Back {
                account,
                key,
                before,
            } => self.put_back(host, now, account, key, before),
            Step::Summaries { account, threads } => {
                self.write_summaries(host, now, account, threads)
            }
        }
    }

    /// What `threads` of `account` add up to, written. Not written, the account says why and the
    /// step waits for the next poll.
    fn write_summaries(
        &mut self,
        host: &Host,
        now: &str,
        account: &str,
        threads: &BTreeSet<String>,
    ) -> Taken {
        let writes = {
            let Some(state) = self.state.as_ref() else {
                return Taken::Nothing;
            };
            let Some(acct) = state.accounts.get(account) else {
                return Taken::Nothing;
            };
            summarize(host, &state.types, acct, threads)
        };
        if writes.is_empty() {
            return Taken::Nothing;
        }
        match host
            .runtime()
            .apply_origin("imap", writes, Some(now.to_owned()))
        {
            Ok(_) => {
                self.settled(account);
                Taken::Wrote
            }
            Err(error) => {
                tracing::warn!(account, %error, "what a conversation adds up to was not written");
                self.unsaved.insert(
                    account.to_owned(),
                    format!("What the mail adds up to could not be saved: {error}"),
                );
                Taken::Refused
            }
        }
    }

    /// What people changed in the fields people change, handed to the servers.
    fn push(&mut self, host: &Host, now: &str) -> bool {
        let keys: Vec<FieldKey> = std::mem::take(&mut *self.changed.borrow_mut());
        let touched = {
            let Some(state) = self.state.as_mut() else {
                return false;
            };
            let t = &state.types;
            let people: BTreeSet<&str> = ["unread", "flagged", "mailbox"]
                .iter()
                .filter_map(|n| t.message.field(n))
                .collect();
            let mut seen = BTreeSet::new();
            let mut touched: HashMap<String, BTreeSet<String>> = HashMap::new();
            for key in keys {
                if !people.contains(key.field.as_str()) && key.field != "*" {
                    continue;
                }
                if !seen.insert(key.resource.clone()) {
                    continue;
                }
                let Some((account, mkey)) = state.by_record.get(&key.resource).cloned() else {
                    continue;
                };
                let Some(acct) = state.accounts.get_mut(&account) else {
                    continue;
                };
                let Some(held) = acct.messages.get_mut(&mkey) else {
                    continue;
                };
                let now_people = People {
                    unread: read_bool(host, &held.record, &t.message, "unread"),
                    flagged: read_bool(host, &held.record, &t.message, "flagged"),
                    mailbox: read_text(host, &held.record, &t.message, "mailbox"),
                };
                if now_people == held.confirmed {
                    continue;
                }
                let before = held.confirmed.clone();
                if (now_people.unread != before.unread || now_people.flagged != before.flagged)
                    && let Some(request) =
                        self.connector
                            .flag(&account, &mkey, now_people.unread, now_people.flagged)
                {
                    self.pending.insert(
                        request,
                        Pending::Write {
                            account: account.clone(),
                            key: mkey.clone(),
                            before: before.clone(),
                        },
                    );
                }
                if now_people.mailbox != before.mailbox
                    && let Some(request) =
                        self.connector.move_to(&account, &mkey, &now_people.mailbox)
                {
                    self.pending.insert(
                        request,
                        Pending::Write {
                            account: account.clone(),
                            key: mkey.clone(),
                            before: before.clone(),
                        },
                    );
                }
                held.confirmed = now_people;
                touched
                    .entry(account)
                    .or_default()
                    .insert(held.thread.clone());
            }
            touched.into_iter().collect::<Vec<_>>()
        };
        let mut wrote = false;
        for (account, threads) in touched {
            let taken = self.write_summaries(host, now, &account, &threads);
            if !taken.done() {
                self.unwritten.keep(Step::Summaries { account, threads });
            }
            wrote |= taken.wrote();
        }
        wrote
    }

    /// A write the server refused or could not be reached for: the message is put back as the
    /// server holds it, and the step waits for the next poll if the workspace cannot take it.
    fn write_back(
        &mut self,
        host: &Host,
        now: &str,
        account: String,
        key: String,
        before: People,
    ) -> bool {
        let taken = self.put_back(host, now, &account, &key, &before);
        if !taken.done() {
            self.unwritten.keep(Step::Back {
                account,
                key,
                before,
            });
        }
        taken.wrote()
    }

    /// A write the server refused: the message is put back as the server holds it. A workspace
    /// that cannot take it says why on the account, and the step waits for the next poll.
    fn put_back(
        &mut self,
        host: &Host,
        now: &str,
        account: &str,
        key: &str,
        before: &People,
    ) -> Taken {
        let writes = {
            let Some(state) = self.state.as_mut() else {
                return Taken::Nothing;
            };
            let t = &state.types;
            let Some(acct) = state.accounts.get_mut(account) else {
                return Taken::Nothing;
            };
            let Some(held) = acct.messages.get_mut(key) else {
                return Taken::Nothing;
            };
            held.confirmed = before.clone();
            let mut writes = vec![people_write(t, &held.record, before)];
            let thread = held.thread.clone();
            writes.extend(summarize(host, t, acct, &BTreeSet::from([thread])));
            writes
        };
        match host
            .runtime()
            .apply_origin("imap", writes, Some(now.to_owned()))
        {
            Ok(_) => {
                self.settled(account);
                Taken::Wrote
            }
            Err(error) => {
                tracing::warn!(account, key, %error, "what the server refused was not put back");
                self.unsaved.insert(
                    account.to_owned(),
                    format!("A message could not be put back as the server holds it: {error}"),
                );
                Taken::Refused
            }
        }
    }

    /// Each account says whether its server answers, what is wrong with it, and why not.
    fn account_state(&self, host: &Host) -> Vec<OriginWrite> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let t = &state.types;
        let mut writes = Vec::new();
        for (id, acct) in &state.accounts {
            let unreached = self.trouble.get(id).map_or("", String::as_str);
            // What is wrong with the account: a server not reached is said before mail that
            // could not be written, which the next poll writes again.
            let trouble = if unreached.is_empty() {
                self.unsaved.get(id).map_or("", String::as_str)
            } else {
                unreached
            };
            let online = unreached.is_empty();
            let fetched = self.fetched.get(id).map_or("", String::as_str);
            if read_text(host, &acct.record, &t.account, "trouble") != trouble
                || read_bool(host, &acct.record, &t.account, "online") != online
                || (!fetched.is_empty()
                    && read_text(host, &acct.record, &t.account, "fetched") != fetched)
            {
                let mut fields = Vec::new();
                put(&t.account, &mut fields, "trouble", text(trouble));
                put(&t.account, &mut fields, "online", Value::Bool(online));
                if !fetched.is_empty() {
                    put(&t.account, &mut fields, "fetched", text(fetched));
                }
                writes.push(OriginWrite::Set {
                    target: acct.record.clone(),
                    fields,
                });
            }
        }
        writes
    }
}

/// One step of the sync: the conversations a server's answer starts, the messages it read and
/// what they add up to, written as one change of the origin, so the workspace keeps all of the
/// step or none of it.
///
/// # Errors
///
/// A store that cannot take the change; then the change is taken back, and the account's record
/// of itself is read again by [`ImapOrigin::take_in`].
fn take_in_step(
    host: &Host,
    now: &str,
    state: &mut State,
    changes: &Changes,
) -> Result<bool, RuntimeError> {
    let t = &state.types;
    let Some(threads_field) = t.account.field("threads") else {
        return Ok(false);
    };
    let Some(acct) = state.accounts.get_mut(&changes.account) else {
        return Ok(false);
    };
    // The messages inserted, by their record and the server's key, to be known by their record
    // once the change is kept.
    let mut inserted: Vec<(String, String)> = Vec::new();
    host.runtime()
        .write_origin("imap", Some(now.to_owned()), |change| {
            let mut touched: BTreeSet<String> = BTreeSet::new();
            // New conversations first: their messages go inside them.
            take_in_threads(change, t, acct, threads_field, changes)?;
            // Then the messages: new ones inserted, known ones brought to what the server says
            // unless someone changed them here and the server has not been told yet.
            let mut writes = Vec::new();
            let mut new_messages: Vec<&ImapMessage> = Vec::new();
            for m in &changes.messages {
                if let Some(held) = acct.messages.get_mut(&m.key) {
                    let server = People {
                        unread: m.unread,
                        flagged: m.flagged,
                        mailbox: m.mailbox.clone(),
                    };
                    if held.confirmed != server {
                        held.confirmed = server.clone();
                        writes.push(people_write(t, &held.record, &server));
                        touched.insert(held.thread.clone());
                    }
                    continue;
                }
                let Some(thread) = acct.threads.get(&m.thread) else {
                    continue;
                };
                let Some(messages_field) = t.thread.field("messages") else {
                    continue;
                };
                let mut fields = message_fields(&t.message, m);
                put(&t.message, &mut fields, "account", text(&changes.account));
                writes.push(OriginWrite::Insert {
                    owner: thread.record.clone(),
                    field: messages_field.to_owned(),
                    ty: t.message.ty.clone(),
                    fields,
                });
                new_messages.push(m);
            }
            for (key, unread, flagged) in &changes.flags {
                if let Some(held) = acct.messages.get_mut(key) {
                    let server = People {
                        unread: *unread,
                        flagged: *flagged,
                        mailbox: held.confirmed.mailbox.clone(),
                    };
                    if held.confirmed != server {
                        held.confirmed = server.clone();
                        writes.push(people_write(t, &held.record, &server));
                        touched.insert(held.thread.clone());
                    }
                }
            }
            // The inserts are in `writes` interleaved with updates; find their results by order.
            let made = change.write(writes)?;
            let mut made_iter = made.into_iter().flatten();
            for m in new_messages {
                let Some(record) = made_iter.next() else {
                    break;
                };
                inserted.push((record.id_string(), m.key.clone()));
                if let Some(thread) = acct.threads.get_mut(&m.thread) {
                    thread.keys.push(m.key.clone());
                }
                touched.insert(m.thread.clone());
                acct.messages.insert(
                    m.key.clone(),
                    Msg {
                        record,
                        thread: m.thread.clone(),
                        received: m.received,
                        from_name: m.from_name.clone(),
                        snippet: snippet_of(&m.body),
                        confirmed: People {
                            unread: m.unread,
                            flagged: m.flagged,
                            mailbox: m.mailbox.clone(),
                        },
                    },
                );
            }
            change.write(summarize(host, t, acct, &touched))?;
            Ok(())
        })?;
    for (record, key) in inserted {
        state
            .by_record
            .insert(record, (changes.account.clone(), key));
    }
    Ok(true)
}

/// The conversations a server's changes start, each a record in its account, in `change`.
fn take_in_threads(
    change: &mut OriginChange<'_>,
    t: &Types,
    acct: &mut Acct,
    threads_field: &str,
    changes: &Changes,
) -> Result<(), RuntimeError> {
    let mut new_threads: Vec<String> = Vec::new();
    let mut writes = Vec::new();
    for m in &changes.messages {
        if acct.messages.contains_key(&m.key)
            || acct.threads.contains_key(&m.thread)
            || new_threads.contains(&m.thread)
        {
            continue;
        }
        let mut fields = Vec::new();
        put(&t.thread, &mut fields, "key", text(&m.thread));
        put(&t.thread, &mut fields, "account", text(&changes.account));
        put(&t.thread, &mut fields, "subject", text(&m.subject));
        writes.push(OriginWrite::Insert {
            owner: acct.record.clone(),
            field: threads_field.to_owned(),
            ty: t.thread.ty.clone(),
            fields,
        });
        new_threads.push(m.thread.clone());
    }
    let made = change.write(writes)?;
    for (key, record) in new_threads.into_iter().zip(made) {
        if let Some(record) = record {
            acct.threads.insert(
                key,
                Thr {
                    record,
                    keys: Vec::new(),
                },
            );
        }
    }
    Ok(())
}

fn people_write(t: &Types, record: &ResourceRef, p: &People) -> OriginWrite {
    let mut fields = Vec::new();
    put(&t.message, &mut fields, "unread", Value::Bool(p.unread));
    put(&t.message, &mut fields, "flagged", Value::Bool(p.flagged));
    put(&t.message, &mut fields, "mailbox", text(&p.mailbox));
    OriginWrite::Set {
        target: record.clone(),
        fields,
    }
}

fn message_fields(ty: &OriginType, m: &ImapMessage) -> Vec<(String, Value)> {
    let mut f = Vec::new();
    put(ty, &mut f, "key", text(&m.key));
    put(ty, &mut f, "subject", text(&m.subject));
    put(ty, &mut f, "from", text(&m.from));
    put(ty, &mut f, "from_name", text(&m.from_name));
    put(ty, &mut f, "to", text(&m.to));
    put(ty, &mut f, "cc", text(&m.cc));
    put(
        ty,
        &mut f,
        "received",
        text(&tilolang_tilo::host::iso_utc(m.received)),
    );
    put(ty, &mut f, "snippet", text(&snippet_of(&m.body)));
    put(ty, &mut f, "body", text(&m.body));
    put(ty, &mut f, "message_id", text(&m.message_id));
    put(ty, &mut f, "references", text(&m.references));
    put(ty, &mut f, "attachments", text(&m.attachments.join("\n")));
    put(ty, &mut f, "unread", Value::Bool(m.unread));
    put(ty, &mut f, "flagged", Value::Bool(m.flagged));
    put(ty, &mut f, "mailbox", text(&m.mailbox));
    f
}

/// What each touched conversation says of its messages: its newest, who wrote, how many,
/// whether one is unread or flagged and which mailboxes they are in; and each mailbox's counts.
fn summarize(host: &Host, t: &Types, acct: &Acct, threads: &BTreeSet<String>) -> Vec<OriginWrite> {
    let mut writes = Vec::new();
    for key in threads {
        let Some(thread) = acct.threads.get(key) else {
            continue;
        };
        let mut msgs: Vec<&Msg> = thread
            .keys
            .iter()
            .filter_map(|k| acct.messages.get(k))
            .collect();
        msgs.sort_by_key(|m| m.received);
        let Some(&newest) = msgs.last() else {
            continue;
        };
        let mut senders: Vec<&str> = Vec::new();
        for m in msgs.iter().rev() {
            if !senders.contains(&m.from_name.as_str()) {
                senders.push(&m.from_name);
            }
        }
        let boxes: BTreeSet<&str> = msgs.iter().map(|m| m.confirmed.mailbox.as_str()).collect();
        let mut fields = Vec::new();
        put(
            &t.thread,
            &mut fields,
            "latest",
            text(&tilolang_tilo::host::iso_utc(newest.received)),
        );
        put(&t.thread, &mut fields, "snippet", text(&newest.snippet));
        put(&t.thread, &mut fields, "senders", text(&senders.join(", ")));
        put(
            &t.thread,
            &mut fields,
            "count",
            Value::Int(i64::try_from(msgs.len()).unwrap_or(i64::MAX)),
        );
        put(
            &t.thread,
            &mut fields,
            "unread",
            Value::Bool(msgs.iter().any(|m| m.confirmed.unread)),
        );
        put(
            &t.thread,
            &mut fields,
            "flagged",
            Value::Bool(msgs.iter().any(|m| m.confirmed.flagged)),
        );
        put(
            &t.thread,
            &mut fields,
            "boxes",
            Value::List(boxes.iter().map(|b| text(b)).collect()),
        );
        writes.push(OriginWrite::Set {
            target: thread.record.clone(),
            fields,
        });
    }
    // The mailboxes' counts.
    let mut unread: BTreeMap<&str, i64> = BTreeMap::new();
    let mut total: BTreeMap<&str, i64> = BTreeMap::new();
    for m in acct.messages.values() {
        let n = total.entry(m.confirmed.mailbox.as_str()).or_default();
        *n = n.saturating_add(1);
        if m.confirmed.unread {
            let u = unread.entry(m.confirmed.mailbox.as_str()).or_default();
            *u = u.saturating_add(1);
        }
    }
    for (key, record) in &acct.boxes {
        let u = unread.get(key.as_str()).copied().unwrap_or(0);
        let n = total.get(key.as_str()).copied().unwrap_or(0);
        let have_u = match read(host, record, &t.mailbox, "unread") {
            Some(Value::Int(i)) => i,
            _ => -1,
        };
        let have_n = match read(host, record, &t.mailbox, "total") {
            Some(Value::Int(i)) => i,
            _ => -1,
        };
        if have_u != u || have_n != n {
            let mut fields = Vec::new();
            put(&t.mailbox, &mut fields, "unread", Value::Int(u));
            put(&t.mailbox, &mut fields, "total", Value::Int(n));
            writes.push(OriginWrite::Set {
                target: record.clone(),
                fields,
            });
        }
    }
    writes
}

/// Seconds since the epoch of an ISO time `YYYY-MM-DDTHH:MM:SSZ`; 0 for anything else, which
/// is said: the record holds a time the origin did not write.
fn seconds_of(iso: &str) -> u64 {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        // Before the epoch is the epoch.
        Ok(at) => u64::try_from(at.timestamp()).unwrap_or(0),
        Err(error) => {
            tracing::warn!(iso, %error, "a message's received time is not a time");
            0
        }
    }
}

impl Origin for ImapOrigin {
    fn name(&self) -> &'static str {
        "imap"
    }

    fn serves(&self, capability: &str) -> bool {
        matches!(capability, "smtp.send" | "imap.check")
    }

    fn due(&self) -> bool {
        self.state.is_none()
            || self.woken.load(Ordering::Acquire)
            || self.reconnected.load(Ordering::Acquire)
            || !self.due.is_empty()
            || !self.unwritten.is_empty()
            || !self.changed.borrow().is_empty()
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        self.woken.store(false, Ordering::Release);
        let mut polled = Polled::default();
        if self.state.is_none() {
            let Some((types, _)) = types_of(cx.host, cx.app) else {
                return polled;
            };
            let Ok(root) = cx.host.root_of(cx.app) else {
                return polled;
            };
            let state = Self::load(cx.host, types, root.clone());
            let changed = Rc::clone(&self.changed);
            let mut store = cx.host.store().clone();
            store.subscribe(
                &root.doc,
                Box::new(move |summary| {
                    changed.borrow_mut().extend(summary.fields.iter().cloned());
                }),
            );
            self.state = Some(state);
            polled.wrote = true;
        }
        // What the workspace could not take last time, before anything newer: a step waits
        // until it is written, never behind newer mail.
        polled.wrote |= self.take_again(cx.host, &cx.now);
        let events = self.take_events(cx.host, &cx.now);
        polled.wrote |= events.wrote;
        polled.settle |= events.settle;
        // The accounts as the connector says them now (one in another process says them
        // with its first answers), then what to ask of each.
        polled.wrote |= self.add_accounts(cx.host, &cx.now);
        polled.settle |= self.reconnect(cx.host);
        for (account, mailbox) in std::mem::take(&mut self.due) {
            if self.connector.changes(&account, &mailbox).is_none() {
                tracing::warn!(
                    account,
                    mailbox,
                    "the mail connector took no request for the account"
                );
            }
        }
        polled.wrote |= self.push(cx.host, &cx.now);
        let states = self.account_state(cx.host);
        if !states.is_empty() {
            // The next poll writes it again: what the account says of itself is read back from
            // it first.
            match cx
                .host
                .runtime()
                .apply_origin("imap", states, Some(cx.now.clone()))
            {
                Ok(_) => polled.wrote = true,
                Err(error) => {
                    tracing::warn!(%error, "what an account says of itself was not written");
                }
            }
        }
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        if capability == "imap.check" {
            return self.check_now(payload.as_str().unwrap_or_default());
        }
        if capability != "smtp.send" {
            return Delivery::Rejected(format!(
                "the mail connector does not carry out {capability}"
            ));
        }
        let key = payload.to_string();
        match self.sends.get(&key) {
            Some(Sending::Going) => return Delivery::Pending,
            Some(Sending::Gone) => {
                self.sends.remove(&key);
                return Delivery::Delivered;
            }
            Some(Sending::Refused(reason)) => {
                let reason = reason.clone();
                self.sends.remove(&key);
                return Delivery::Rejected(reason);
            }
            Some(Sending::Unsent(reason)) => {
                let reason = reason.clone();
                self.sends.remove(&key);
                return Delivery::Offline(reason);
            }
            None => {}
        }
        let field = |i: usize| -> String {
            payload
                .get(i)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let account = field(0);
        let message = Outgoing {
            to: field(1),
            cc: field(2),
            subject: field(3),
            body: field(4),
            in_reply_to: field(5),
            references: field(6),
        };
        match self.connector.send(&account, message) {
            Ok(request) => {
                // Resolved by the connector's answer to `request` (`Accepted`, `Refused` or
                // `Unsent`, which it gives for every request, the engine stopping included):
                // the poll that takes it in settles again.
                self.pending.insert(request, Pending::Send(key.clone()));
                self.sends.insert(key, Sending::Going);
                Delivery::Pending
            }
            Err(Unmade::NoAccount) => {
                Delivery::Rejected(format!("no mail account {account} on this device"))
            }
            // The message never left: it waits to go online, as one the server was not reached
            // for does.
            Err(Unmade::EngineStopped) => Delivery::Offline("the mail engine stopped".to_owned()),
        }
    }
}
