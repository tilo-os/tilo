//! The `CalDAV` origin: keeps a package's types of the `caldav` kinds (`type Event (origin:
//! caldav.event)`) in step with the servers of the workspace's calendar accounts, through the
//! connector (`super::connector`), and carries out `caldav.check`, `caldav.answer` and
//! `caldav.import`.
//!
//! - What a server holds is written into the package's records as changes of the origin: an
//!   account's calendars and their entries, keyed by the ids the server gives them so nothing
//!   appears twice.
//! - What people change in an entry's fields applies at once in the record and is handed to the
//!   server; a refusal puts the fields back and says why on the account.
//! - An entry whose calendar is the workspace's own (one no account holds) is an ordinary
//!   record: the origin never syncs it and never sends it. Moving it into an account's calendar
//!   writes it to that server; moving it back out takes it out there.
//! - A person asking for their calendars again (`caldav.check`) asks the account's server now,
//!   and the account's own fields say how it answered.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::runtime::RuntimeError;
use tilolang_tilo::runtime::origin::{OriginWrite, Unwritten};
use tilolang_tilo::runtime::store::{FieldKey, Store as _};
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use super::connector::{CalDavConnector, CalendarRef, Entry, Event, Request, Sync, Unmade};

/// What every writing to a read-only account is refused with.
pub const READ_ONLY: &str = "This calendar account is connected for reading only, so nothing is \
                            written to its calendar server.";

/// The fields of an entry people change.
const PEOPLE: &[&str] = &[
    "title",
    "start",
    "end",
    "all_day",
    "timezone",
    "location",
    "repeats",
    "repeat_every",
    "repeat_until",
    "repeat_count",
    "repeat_exdates",
    "overrides",
    "organizer",
    "attendees",
    "categories",
    "notes",
    "calendar",
];

#[derive(Clone)]
struct Types {
    /// The root's collection of accounts.
    accounts_field: String,
    /// The root's collection of calendars.
    calendars_field: String,
    /// The root's collection of entries.
    events_field: String,
    account: OriginType,
    calendar: OriginType,
    event: OriginType,
}

struct Acct {
    record: ResourceRef,
    /// The calendars of the account, by their key: the collection's address.
    calendars: HashMap<String, ResourceRef>,
    read_only: bool,
}

#[derive(Clone)]
struct Ev {
    account: String,
    calendar: String,
    href: String,
    /// What the record said when it was last written: what the server holds, as far as it was
    /// told.
    confirmed: Entry,
}

struct State {
    types: Types,
    root: ResourceRef,
    accounts: HashMap<String, Acct>,
    /// The account each calendar of a server belongs to, by its key.
    calendars: HashMap<String, String>,
    /// Each entry the origin took in, by its record's id.
    events: HashMap<String, Ev>,
    /// The record of each entry of each account, by the account and its href.
    by_href: HashMap<(String, String), String>,
    /// The record of each entry of each account, by the account and its `UID`.
    by_uid: HashMap<(String, String), String>,
}

impl State {
    /// The record of the entry `uid` of `account`.
    fn record_of(&self, account: &str, uid: &str) -> Option<String> {
        self.by_uid
            .get(&(account.to_owned(), uid.to_owned()))
            .cloned()
    }

    /// Whether `calendar` is an account's, and which.
    fn calendar_of(&self, calendar: &str) -> Option<&str> {
        self.calendars
            .get(calendar)
            .filter(|account| !account.is_empty())
            .map(String::as_str)
    }

    fn forget(&mut self, record: &str) {
        if let Some(ev) = self.events.remove(record) {
            self.by_href.remove(&(ev.account.clone(), ev.href.clone()));
            self.by_uid.remove(&(ev.account, ev.confirmed.uid));
        }
    }
}

/// A request handed to a server and not answered yet.
enum Pending {
    /// An entry written; `before` is what the record said of the server before it was changed.
    Put { record: String, before: Entry },
    /// An entry taken out, by its record: it moves to the workspace's own calendar.
    Remove { record: String, before: Entry },
    /// An effect of a capability, by the payload that asked for it.
    Asked(String),
}

enum Sending {
    Going,
    Gone,
    Refused(String),
    /// The server could not be reached: the ask waits to go online.
    Unsent(String),
}

/// A step of the sync the workspace could not take, kept to be written again by the next poll.
enum Step {
    /// What a server said changed in an account.
    In(Box<Sync>),
    /// An entry's fields put back as the server holds them, after it refused them.
    Restore {
        account: String,
        record: String,
        entry: Entry,
    },
}

impl Step {
    fn account(&self) -> &str {
        match self {
            Self::In(sync) => &sync.account,
            Self::Restore { account, .. } => account,
        }
    }
}

/// What the workspace made of one step of the sync.
enum Taken {
    Wrote,
    Nothing,
    Refused,
}

impl Taken {
    fn done(&self) -> bool {
        !matches!(self, Self::Refused)
    }

    fn wrote(&self) -> bool {
        matches!(self, Self::Wrote)
    }
}

/// The origin polling, until dropped.
struct Polling(Rc<std::cell::Cell<bool>>);

impl Polling {
    fn start(polling: &Rc<std::cell::Cell<bool>>) -> Self {
        polling.set(true);
        Self(Rc::clone(polling))
    }
}

impl Drop for Polling {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// The origin. See the module docs.
pub struct CalDavOrigin {
    connector: Box<dyn CalDavConnector>,
    /// Set when the connector has an answer, from the thread it answers on.
    woken: Arc<AtomicBool>,
    state: Option<State>,
    /// Whether the workspace must be read again before anything is taken into it.
    reload: bool,
    /// Whether the settings name an account the workspace was not read as holding yet.
    accounts_waiting: bool,
    /// What changed in the room since the last poll, as the store told it.
    changed: Rc<RefCell<Vec<FieldKey>>>,
    subscribed: bool,
    /// Set while the origin polls: what the store tells then is its own writing.
    polling: Rc<std::cell::Cell<bool>>,
    pending: BTreeMap<Request, Pending>,
    /// The outcome of each `caldav.answer` or `caldav.import`, by the payload that asked.
    sends: HashMap<String, Sending>,
    /// Why each account's server could not be reached, until it can.
    trouble: HashMap<String, String>,
    /// Why each account's calendars could not be written into the workspace, until they can be.
    unsaved: HashMap<String, String>,
    /// When each account's calendars were last taken in: what the account says of itself.
    fetched: HashMap<String, String>,
    /// Why the last write of each entry was refused, until the entry is written again.
    refused: HashMap<String, String>,
    /// The steps the workspace could not take, oldest first.
    unwritten: Unwritten<Step>,
    /// The accounts whose changes to take in.
    due: Vec<String>,
    /// The accounts a sync was asked for, so one that fails is not asked again every poll.
    asked: HashSet<String>,
    /// The records moved to another account's calendar, written to it once the server took them
    /// out of the first.
    moving: Vec<String>,
    /// Set when the device came online again.
    reconnected: Arc<AtomicBool>,
}

impl CalDavOrigin {
    /// The origin over `connector`, which sets `woken` when it has an answer.
    #[must_use]
    pub fn new(connector: Box<dyn CalDavConnector>, woken: Arc<AtomicBool>) -> Self {
        Self {
            connector,
            woken,
            state: None,
            reload: false,
            accounts_waiting: false,
            subscribed: false,
            changed: Rc::default(),
            polling: Rc::default(),
            pending: BTreeMap::new(),
            sends: HashMap::new(),
            trouble: HashMap::new(),
            unsaved: HashMap::new(),
            fetched: HashMap::new(),
            refused: HashMap::new(),
            unwritten: Unwritten::new(),
            due: Vec::new(),
            asked: HashSet::new(),
            moving: Vec::new(),
            reconnected: Arc::default(),
        }
    }

    /// This origin, trying again what could not reach its server whenever `reconnected` is set
    /// (the device came online again) and it is woken.
    #[must_use]
    pub fn reconnecting_on(mut self, reconnected: Arc<AtomicBool>) -> Self {
        self.reconnected = reconnected;
        self
    }

    fn reconnect(&mut self, host: &Host) -> bool {
        if !self.reconnected.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.asked.clear();
        let accounts: Vec<String> = self
            .state
            .as_ref()
            .map_or_else(Vec::new, |state| state.accounts.keys().cloned().collect());
        for account in accounts {
            self.due.push(account);
        }
        host.runtime().retry_waiting();
        true
    }

    /// The account's server asked now, as a person asking for their calendars again does.
    fn check_now(&mut self, account: &str) -> Delivery {
        let known = self
            .state
            .as_ref()
            .is_some_and(|state| state.accounts.contains_key(account));
        if !known {
            return Delivery::Rejected(format!("no calendar account {account} on this device"));
        }
        self.asked.remove(account);
        self.due.push(account.to_owned());
        Delivery::Delivered
    }

    /// What the connector reported since the last poll.
    fn take_events(&mut self, host: &Host, now: &str) -> Polled {
        let mut polled = Polled::default();
        for event in self.connector.events() {
            match event {
                Event::Synced(sync) => {
                    self.fetched.insert(sync.account.clone(), now.to_owned());
                    self.trouble.remove(&sync.account);
                    polled.wrote |= self.take_sync(host, now, sync).wrote();
                }
                Event::Unreachable { account, reason } => {
                    self.trouble.insert(account, reason);
                }
                Event::Accepted { request, href } => match self.pending.remove(&request) {
                    Some(Pending::Put { record, .. }) => {
                        polled.wrote |= self.stored(host, now, &record, &href);
                    }
                    Some(Pending::Remove { record, .. }) => {
                        polled.wrote |= self.taken_out(host, now, &record);
                    }
                    Some(Pending::Asked(key)) => {
                        self.sends.insert(key, Sending::Gone);
                        polled.settle = true;
                    }
                    None => {}
                },
                Event::Refused { request, reason } => match self.pending.remove(&request) {
                    Some(Pending::Put { record, before } | Pending::Remove { record, before }) => {
                        polled.wrote |= self.refused(host, now, &record, &before, &reason);
                    }
                    Some(Pending::Asked(key)) => {
                        self.sends.insert(key, Sending::Refused(reason));
                        polled.settle = true;
                    }
                    None => {}
                },
                Event::Unsent { request, reason } => {
                    match self.pending.remove(&request) {
                        Some(
                            Pending::Put { record, before } | Pending::Remove { record, before },
                        ) => {
                            // It never reached its server: the record keeps its fields and its
                            // place, and the account says why it could not go.
                            polled.wrote |= self.refused(host, now, &record, &before, &reason);
                        }
                        Some(Pending::Asked(key)) => {
                            self.sends.insert(key, Sending::Unsent(reason));
                            polled.settle = true;
                        }
                        None => {}
                    }
                }
            }
        }
        polled
    }

    /// An entry the server took: its record says where it is now, and what was written is what
    /// the server holds.
    fn stored(&mut self, host: &Host, now: &str, record: &str, href: &str) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let Some(ev) = state.events.get_mut(record) else {
            return false;
        };
        let was = std::mem::replace(&mut ev.href, href.to_owned());
        href.clone_into(&mut ev.confirmed.href);
        let account = ev.account.clone();
        let uid = ev.confirmed.uid.clone();
        state.by_href.remove(&(account.clone(), was));
        state
            .by_href
            .insert((account.clone(), href.to_owned()), record.to_owned());
        state
            .by_uid
            .insert((ev.account.clone(), uid), record.to_owned());
        self.refused.remove(record);
        self.settled(&account);
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let t = &state.types;
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.event, &mut fields, "href", text(href));
        matches!(
            host.runtime().apply_origin(
                "caldav",
                vec![OriginWrite::Set { target: at, fields }],
                Some(now.to_owned())
            ),
            Ok(_)
        )
    }

    /// An entry the server took out: it is no longer on the server, and its record says so,
    /// keeping every field the person gave it, for the calendar it is in now.
    fn taken_out(&mut self, host: &Host, now: &str, record: &str) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let Some(ev) = state.events.get_mut(record) else {
            return false;
        };
        let account = ev.account.clone();
        let href = ev.href.clone();
        ev.href.clear();
        state.by_href.remove(&(account.clone(), href));
        self.settled(&account);
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let t = &state.types;
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.event, &mut fields, "href", text(""));
        matches!(
            host.runtime().apply_origin(
                "caldav",
                vec![OriginWrite::Set { target: at, fields }],
                Some(now.to_owned())
            ),
            Ok(_)
        )
    }

    /// The server refused a change, or was not reached: the entry's fields go back to what the
    /// server holds, and the account says why.
    fn refused(
        &mut self,
        host: &Host,
        now: &str,
        record: &str,
        before: &Entry,
        reason: &str,
    ) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let Some(ev) = state.events.get_mut(record) else {
            return false;
        };
        let account = ev.account.clone();
        ev.confirmed = before.clone();
        ev.calendar.clone_from(&before.calendar);
        if !before.href.is_empty() {
            ev.href.clone_from(&before.href);
        }
        self.unsaved.insert(account.clone(), reason.to_owned());
        self.refused.insert(record.to_owned(), reason.to_owned());
        let t = state.types.clone();
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let writes = vec![OriginWrite::Set {
            target: at,
            fields: entry_fields(&t.event, before),
        }];
        let taken = self.put_step(host, now, &account.clone(), writes);
        if !taken.done() {
            // The workspace would not take what the server holds: it is written again by the
            // next poll, before anything newer.
            self.unwritten.keep(Step::Restore {
                account,
                record: record.to_owned(),
                entry: before.clone(),
            });
        }
        taken.wrote()
    }

    /// A sync the workspace would not take is kept, and what a server said is not lost.
    fn take_sync(&mut self, host: &Host, now: &str, sync: Sync) -> Taken {
        match self.take_in(host, now, &sync) {
            Ok(wrote) => {
                self.settled(&sync.account);
                if wrote { Taken::Wrote } else { Taken::Nothing }
            }
            Err(error) => {
                if self
                    .state
                    .as_ref()
                    .is_some_and(|state| !state.accounts.contains_key(&sync.account))
                {
                    return Taken::Refused;
                }
                tracing::warn!(account = %sync.account, %error, "what a calendar server said could not be written");
                self.unsaved.insert(
                    sync.account.clone(),
                    format!("The calendars could not be saved: {error}"),
                );
                self.keep(sync);
                Taken::Refused
            }
        }
    }

    fn keep(&mut self, sync: Sync) {
        let account = sync.account.clone();
        if let Some(Step::In(held)) = self
            .unwritten
            .waiting_mut(|step| matches!(step, Step::In(held) if held.account == account))
        {
            for entry in sync.entries {
                if !held
                    .entries
                    .iter()
                    .any(|known| known.uid == entry.uid && known.calendar == entry.calendar)
                {
                    held.entries.push(entry);
                }
            }
            for removal in sync.removed {
                if !held
                    .removed
                    .iter()
                    .any(|known| known.href == removal.href && known.calendar == removal.calendar)
                {
                    held.removed.push(removal);
                }
            }
        } else {
            self.unwritten.keep(Step::In(Box::new(sync)));
        }
    }

    /// The account's calendars are written: while no step of it waits, the account says nothing.
    fn settled(&mut self, account: &str) {
        if !self.unwritten.waits(|step| step.account() == account) {
            self.unsaved.remove(account);
        }
    }

    fn take_in(&mut self, host: &Host, now: &str, sync: &Sync) -> Result<bool, RuntimeError> {
        if self.reload {
            return Err(RuntimeError::NotFound(
                "the calendars are taken in once the workspace is read again".into(),
            ));
        }
        let pending = &self.pending;
        let Some(state) = self.state.as_mut() else {
            return Ok(false);
        };
        write_sync(host, now, state, pending, sync)
    }

    fn take_again(&mut self, host: &Host, now: &str) -> bool {
        let mut wrote = false;
        let mut unwritten = std::mem::take(&mut self.unwritten);
        unwritten.take_again(|step| {
            let taken = match step {
                Step::In(sync) => self.take_sync(host, now, sync.as_ref().clone()),
                Step::Restore {
                    account,
                    record,
                    entry,
                } => {
                    let Some(state) = self.state.as_ref() else {
                        return false;
                    };
                    let t = state.types.clone();
                    let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
                        return false;
                    };
                    let writes = vec![OriginWrite::Set {
                        target: at,
                        fields: entry_fields(&t.event, entry),
                    }];
                    let account = account.clone();
                    self.put_step(host, now, &account, writes)
                }
            };
            wrote |= taken.wrote();
            taken.done()
        });
        self.unwritten = unwritten;
        wrote
    }

    /// Writes go through one place, so a step the workspace cannot take says why on its account.
    fn put_step(
        &mut self,
        host: &Host,
        now: &str,
        account: &str,
        writes: Vec<OriginWrite>,
    ) -> Taken {
        match host
            .runtime()
            .apply_origin("caldav", writes, Some(now.to_owned()))
        {
            Ok(_) => {
                self.settled(account);
                Taken::Wrote
            }
            Err(error) => {
                tracing::warn!(account, %error, "what the calendar says was not written");
                self.unsaved.insert(
                    account.to_owned(),
                    format!("What the calendar says could not be saved: {error}"),
                );
                Taken::Refused
            }
        }
    }

    /// What people changed in the fields people change, handed to the servers.
    fn push(&mut self, host: &Host, now: &str) -> bool {
        if self.state.is_none() {
            return false;
        }
        let keys: Vec<FieldKey> = std::mem::take(&mut *self.changed.borrow_mut());
        let mut touched: BTreeSet<String> = BTreeSet::new();
        {
            let Some(state) = self.state.as_ref() else {
                return false;
            };
            let t = &state.types;
            let people: BTreeSet<&str> = PEOPLE
                .iter()
                .copied()
                .filter(|name| t.event.field(name).is_some())
                .collect();
            for key in keys {
                if !people.contains(key.field.as_str()) && key.field != "*" {
                    continue;
                }
                touched.insert(key.resource);
            }
        }
        let mut wrote = false;
        for record in touched {
            wrote |= self.pushed(host, now, &record);
        }
        wrote
    }

    /// What one entry's record says now, handed to its server.
    fn pushed(&mut self, host: &Host, now: &str, record: &str) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let t = state.types.clone();
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let known = state.events.contains_key(record);
        let calendar = match try_read_text(host, &at, &t.event, "calendar") {
            Ok(calendar) => calendar,
            // The record is gone: the entry goes from its server, where it is on one.
            Err(_) => {
                return known && self.remove_from_server(host, now, record);
            }
        };
        let Ok(current) = try_entry(host, &at, &t.event) else {
            return known && self.remove_from_server(host, now, record);
        };
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let held = state.events.get(record).cloned();
        let Some(target_account) = state.calendar_of(&calendar).map(str::to_owned) else {
            // The workspace's own calendar: the origin never sends it. One that was on a
            // server is taken out there, and its fields stay as the person left them.
            return known && self.remove_from_server(host, now, record);
        };
        let Some(held) = held else {
            // An entry the origin never took in: it was made here, in an account's calendar.
            return self.write_to_server(host, now, record, &current, &target_account, None);
        };
        let moved = held.calendar != calendar;
        if moved && target_account != held.account {
            // Another account's calendar: out of the first server first, then written to the
            // other's once it went.
            if !self.remove_from_server(host, now, record) {
                return false;
            }
            self.moving.push(record.to_owned());
            return true;
        }
        if !moved && same_entry(&current, &held.confirmed) {
            return false;
        }
        // Another calendar of the same server: it goes from where it was and is written as a new
        // entry in the calendar the person chose.
        let from = moved.then_some(held.href.as_str());
        if self.write_to_server(host, now, record, &current, &target_account, from) {
            return true;
        }
        // Nothing was written: what the record says is put back where the server refused it, or
        // where the account's read-only rule did.
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let Some(ev) = state.events.get(record) else {
            return false;
        };
        let account = ev.account.clone();
        let before = ev.confirmed.clone();
        let t = state.types.clone();
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let writes = vec![OriginWrite::Set {
            target: at,
            fields: entry_fields(&t.event, &before),
        }];
        self.put_step(host, now, &account, writes).wrote()
    }

    /// The entry `record` as its server should hold it: written where it is, or as a new entry,
    /// taking it out of `also_remove` first.
    fn write_to_server(
        &mut self,
        host: &Host,
        now: &str,
        record: &str,
        entry: &Entry,
        account: &str,
        also_remove: Option<&str>,
    ) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let Some(acct) = state.accounts.get(account) else {
            return false;
        };
        if acct.read_only {
            // Never asked: a read-only account refuses before any server is.
            self.unsaved
                .insert(account.to_owned(), READ_ONLY.to_owned());
            self.refused.insert(record.to_owned(), READ_ONLY.to_owned());
            return false;
        }
        let t = state.types.clone();
        let Some(ev) = state.events.get(record) else {
            // A new entry: its record has no href yet.
            let mut entry = entry.clone();
            if entry.uid.is_empty() {
                entry.uid = uuid::Uuid::new_v4().to_string();
            }
            let before = entry.clone();
            let request = self
                .connector
                .put(account, entry.clone(), "", also_remove.unwrap_or(""));
            let Ok(request) = request else {
                self.unsaved
                    .insert(account.to_owned(), "The calendar engine stopped".to_owned());
                return false;
            };
            self.pending.insert(
                request,
                Pending::Put {
                    record: record.to_owned(),
                    before,
                },
            );
            if let Some(state) = self.state.as_mut() {
                state.events.insert(
                    record.to_owned(),
                    Ev {
                        account: account.to_owned(),
                        calendar: entry.calendar.clone(),
                        href: String::new(),
                        confirmed: entry.clone(),
                    },
                );
            }
            if let Some(at) = ResourceRef::parse_id(record, &t.event.ty) {
                let mut fields = Vec::new();
                put(&t.event, &mut fields, "key", text(&entry.uid));
                let _ = host.runtime().apply_origin(
                    "caldav",
                    vec![OriginWrite::Set { target: at, fields }],
                    Some(now.to_owned()),
                );
            }
            return true;
        };
        let before = ev.confirmed.clone();
        let href = if also_remove.is_some() {
            ""
        } else {
            ev.href.as_str()
        };
        let request = self
            .connector
            .put(account, entry.clone(), href, also_remove.unwrap_or(""));
        let Ok(request) = request else {
            self.unsaved
                .insert(account.to_owned(), "The calendar engine stopped".to_owned());
            return false;
        };
        self.pending.insert(
            request,
            Pending::Put {
                record: record.to_owned(),
                before,
            },
        );
        if let Some(state) = self.state.as_mut()
            && let Some(ev) = state.events.get_mut(record)
        {
            ev.calendar.clone_from(&entry.calendar);
            ev.confirmed = entry.clone();
            if also_remove.is_some() {
                ev.href.clear();
            }
        }
        true
    }

    /// The entry `record` taken out of its server: it moved to the workspace's own calendar, or
    /// its record is gone.
    fn remove_from_server(&mut self, host: &Host, now: &str, record: &str) -> bool {
        let _ = (host, now);
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let Some(ev) = state.events.get(record) else {
            return false;
        };
        if ev.href.is_empty() {
            return false;
        }
        let account = ev.account.clone();
        let calendar = ev.calendar.clone();
        let href = ev.href.clone();
        let before = ev.confirmed.clone();
        if state
            .accounts
            .get(&account)
            .is_none_or(|acct| acct.read_only)
        {
            self.unsaved.insert(account, READ_ONLY.to_owned());
            return false;
        }
        match self.connector.remove(&account, &calendar, &href) {
            Ok(request) => {
                self.pending.insert(
                    request,
                    Pending::Remove {
                        record: record.to_owned(),
                        before,
                    },
                );
                true
            }
            Err(Unmade::NoAccount) => {
                self.unsaved.insert(
                    account,
                    "no calendar account on this device is that one".to_owned(),
                );
                false
            }
            Err(Unmade::EngineStopped) => {
                self.unsaved
                    .insert(account, "The calendar engine stopped".to_owned());
                false
            }
        }
    }

    /// The records moved to another account's calendar are written there, now that the first
    /// server took them out.
    fn moved(&mut self, host: &Host, now: &str) -> bool {
        let moving = std::mem::take(&mut self.moving);
        let mut wrote = false;
        for record in moving {
            let Some(state) = self.state.as_ref() else {
                break;
            };
            let t = state.types.clone();
            let Some(at) = ResourceRef::parse_id(&record, &t.event.ty) else {
                continue;
            };
            let Ok(current) = try_entry(host, &at, &t.event) else {
                continue;
            };
            let Some(account) = state.calendar_of(&current.calendar).map(str::to_owned) else {
                continue;
            };
            wrote |= self.write_to_server(host, now, &record, &current, &account, None);
        }
        wrote
    }

    /// What each account says of itself is written, and whether it was.
    fn write_account_state(&self, host: &Host, now: &str) -> bool {
        let states = self.account_state(host);
        if states.is_empty() {
            return false;
        }
        match host
            .runtime()
            .apply_origin("caldav", states, Some(now.to_owned()))
        {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, "what an account says of itself was not written");
                false
            }
        }
    }

    /// Each account says whether its server answers, what is wrong with it, and when its
    /// calendars were last taken in.
    fn account_state(&self, host: &Host) -> Vec<OriginWrite> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let t = &state.types;
        let mut writes = Vec::new();
        for (id, acct) in &state.accounts {
            let unreached = self.trouble.get(id).map_or("", String::as_str);
            let trouble = if unreached.is_empty() {
                self.unsaved.get(id).map_or("", String::as_str)
            } else {
                unreached
            };
            let fetched = self.fetched.get(id).map_or("", String::as_str);
            if read_text(host, &acct.record, &t.account, "status") != trouble
                || (!fetched.is_empty()
                    && read_text(host, &acct.record, &t.account, "last_synced") != fetched)
            {
                let mut fields = Vec::new();
                put(&t.account, &mut fields, "status", text(trouble));
                if !fetched.is_empty() {
                    put(&t.account, &mut fields, "last_synced", text(fetched));
                }
                writes.push(OriginWrite::Set {
                    target: acct.record.clone(),
                    fields,
                });
            }
        }
        writes
    }

    /// Each account of the connector has its record, and its server is asked what it holds.
    fn add_accounts(&mut self, host: &Host, now: &str) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        if !host.store().settled() {
            self.accounts_waiting = true;
            return false;
        }
        self.accounts_waiting = false;
        let t = &state.types;
        let mut writes = Vec::new();
        let mut new_accounts = Vec::new();
        let mut held: BTreeSet<String> = BTreeSet::new();
        for (key, acct) in &state.accounts {
            held.insert(identity(key));
            held.insert(identity(&read_text(
                host,
                &acct.record,
                &t.account,
                "address",
            )));
        }
        for a in self.connector.accounts() {
            let ids = [identity(&a.id), identity(&a.address)];
            if ids.iter().any(|id| held.contains(id)) {
                continue;
            }
            let mut fields = Vec::new();
            put(&t.account, &mut fields, "key", text(&a.id));
            put(&t.account, &mut fields, "address", text(&a.address));
            put(&t.account, &mut fields, "server", text(&a.server));
            put(
                &t.account,
                &mut fields,
                "read_only",
                Value::Bool(a.read_only),
            );
            put(&t.account, &mut fields, "status", text(""));
            writes.push(OriginWrite::Insert {
                owner: state.root.clone(),
                field: t.accounts_field.clone(),
                ty: t.account.ty.clone(),
                fields,
            });
            held.extend(ids);
            new_accounts.push(a.id);
        }
        if writes.is_empty() {
            return false;
        }
        let made = match write_accounts(host, now, t, writes, new_accounts) {
            Ok(made) => made,
            Err(error) => {
                tracing::warn!(%error, "the calendar accounts could not be written");
                return false;
            }
        };
        let mut made_it = false;
        for (id, record, read_only) in made {
            state.accounts.insert(
                id.clone(),
                Acct {
                    record,
                    calendars: HashMap::new(),
                    read_only,
                },
            );
            self.asked.remove(&id);
            self.due.push(id);
            made_it = true;
        }
        made_it
    }
}

/// The events of the accounts whose changes were not asked for yet are asked for.
fn due_syncs(state: &State, asked: &HashSet<String>) -> Vec<String> {
    state
        .accounts
        .keys()
        .filter(|id| !asked.contains(*id))
        .cloned()
        .collect()
}

/// The accounts' records and their read-only rule, written as one change of the origin.
fn write_accounts(
    host: &Host,
    now: &str,
    t: &Types,
    writes: Vec<OriginWrite>,
    new_accounts: Vec<String>,
) -> Result<Vec<(String, ResourceRef, bool)>, RuntimeError> {
    host.runtime()
        .write_origin("caldav", Some(now.to_owned()), |change| {
            let read_only: HashMap<String, bool> = writes
                .iter()
                .filter_map(|write| match write {
                    OriginWrite::Insert { fields, .. } => {
                        let key = fields
                            .iter()
                            .find(|(id, _)| t.account.field("key") == Some(id.as_str()));
                        let rule = fields
                            .iter()
                            .find(|(id, _)| t.account.field("read_only") == Some(id.as_str()));
                        Some((
                            match key.map(|(_, value)| value) {
                                Some(Value::Text(key)) => key.clone(),
                                _ => String::new(),
                            },
                            matches!(rule.map(|(_, value)| value), Some(Value::Bool(true))),
                        ))
                    }
                    OriginWrite::Set { .. } | OriginWrite::Remove { .. } => None,
                })
                .collect();
            let records = change.write(writes)?;
            let mut accounts = Vec::new();
            for (id, record) in new_accounts.into_iter().zip(records) {
                if let Some(record) = record {
                    let rule = read_only.get(&id).copied().unwrap_or(false);
                    accounts.push((id, record, rule));
                }
            }
            Ok(accounts)
        })
}

/// What a server said changed in `account`, written as one change of the origin.
fn write_sync(
    host: &Host,
    now: &str,
    state: &mut State,
    pending: &BTreeMap<Request, Pending>,
    sync: &Sync,
) -> Result<bool, RuntimeError> {
    let t = state.types.clone();
    let Some(acct) = state.accounts.get(&sync.account) else {
        return Err(RuntimeError::NotFound(format!(
            "no calendar account {} in the workspace as it was read",
            sync.account
        )));
    };
    let mut calendars = acct.calendars.clone();
    // Calendars the server no longer lists, and the entries of each: they go.
    let listed: HashSet<&str> = sync.calendars.iter().map(|cal| cal.key.as_str()).collect();
    let gone_calendars: Vec<String> = calendars
        .keys()
        .filter(|key| !listed.contains(key.as_str()))
        .cloned()
        .collect();
    let gone_events: Vec<String> = gone_calendars
        .iter()
        .flat_map(|calendar| {
            state
                .events
                .iter()
                .filter(move |(_, ev)| &ev.calendar == calendar)
                .map(|(record, _)| record.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    // What the entries of this pull are written as, and which of them are new.
    let mut inserts: Vec<Entry> = Vec::new();
    let mut updates: Vec<(String, Entry)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for entry in &sync.entries {
        if waiting(state, pending, &sync.account, &entry.uid) {
            continue;
        }
        if let Some(record) = state
            .by_href
            .get(&(sync.account.clone(), entry.href.clone()))
            .cloned()
        {
            updates.push((record, entry.clone()));
        } else if let Some(record) = state.record_of(&sync.account, &entry.uid) {
            updates.push((record, entry.clone()));
        } else {
            inserts.push(entry.clone());
        }
    }
    for removal in &sync.removed {
        if let Some(record) = state
            .by_href
            .get(&(sync.account.clone(), removal.href.clone()))
            .cloned()
        {
            removed.push(record);
        }
    }
    let root = state.root.clone();
    let (new_calendars, new_events) =
        host.runtime()
            .write_origin("caldav", Some(now.to_owned()), |change| {
                let mut calendar_writes = Vec::new();
                let mut kept: Vec<String> = Vec::new();
                for cal in &sync.calendars {
                    if calendars.contains_key(&cal.key) {
                        continue;
                    }
                    let mut fields = calendar_fields(&t.calendar, cal);
                    put(&t.calendar, &mut fields, "key", text(&cal.key));
                    calendar_writes.push(OriginWrite::Insert {
                        owner: root.clone(),
                        field: t.calendars_field.clone(),
                        ty: t.calendar.ty.clone(),
                        fields,
                    });
                    kept.push(cal.key.clone());
                }
                let made_calendars = change.write(calendar_writes)?;

                let mut writes = Vec::new();
                for cal in &sync.calendars {
                    if let Some(record) = calendars.get(&cal.key) {
                        let fields = calendar_fields(&t.calendar, cal);
                        if fields_differ(host, record, &t.calendar, &fields) {
                            writes.push(OriginWrite::Set {
                                target: record.clone(),
                                fields,
                            });
                        }
                    }
                }
                for calendar in &gone_calendars {
                    if let Some(record) = calendars.get(calendar) {
                        writes.push(OriginWrite::Remove {
                            owner: root.clone(),
                            field: t.calendars_field.clone(),
                            item: record.clone(),
                        });
                    }
                }
                for record in &gone_events {
                    if let Some(at) = ResourceRef::parse_id(record, &t.event.ty) {
                        writes.push(OriginWrite::Remove {
                            owner: root.clone(),
                            field: t.events_field.clone(),
                            item: at,
                        });
                    }
                }
                for (record, entry) in &updates {
                    let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
                        continue;
                    };
                    writes.push(OriginWrite::Set {
                        target: at,
                        fields: entry_fields(&t.event, entry),
                    });
                }
                for record in &removed {
                    if let Some(at) = ResourceRef::parse_id(record, &t.event.ty) {
                        writes.push(OriginWrite::Remove {
                            owner: root.clone(),
                            field: t.events_field.clone(),
                            item: at,
                        });
                    }
                }
                change.write(writes)?;

                let mut insert_writes = Vec::new();
                for entry in &inserts {
                    insert_writes.push(OriginWrite::Insert {
                        owner: root.clone(),
                        field: t.events_field.clone(),
                        ty: t.event.ty.clone(),
                        fields: entry_fields(&t.event, entry),
                    });
                }
                let made_events = change.write(insert_writes)?;
                Ok((
                    kept.into_iter().zip(made_calendars).collect::<Vec<_>>(),
                    made_events,
                ))
            })?;
    for (key, record) in new_calendars {
        if let Some(record) = record {
            calendars.insert(key, record);
        }
    }
    if let Some(acct) = state.accounts.get_mut(&sync.account) {
        acct.calendars = calendars;
    }
    for record in &gone_events {
        state.forget(record);
    }
    for (record, entry) in &updates {
        if let Some(ev) = state.events.get_mut(record) {
            ev.calendar.clone_from(&entry.calendar);
            ev.href.clone_from(&entry.href);
            ev.confirmed = entry.clone();
            state
                .by_href
                .insert((sync.account.clone(), entry.href.clone()), record.clone());
            state
                .by_uid
                .insert((sync.account.clone(), entry.uid.clone()), record.clone());
        } else {
            let account = sync.account.clone();
            state
                .by_href
                .insert((account.clone(), entry.href.clone()), record.clone());
            state
                .by_uid
                .insert((account.clone(), entry.uid.clone()), record.clone());
            state.events.insert(
                record.clone(),
                Ev {
                    account,
                    calendar: entry.calendar.clone(),
                    href: entry.href.clone(),
                    confirmed: entry.clone(),
                },
            );
        }
    }
    for (entry, record) in inserts.iter().zip(new_events) {
        let Some(record) = record else { continue };
        let record = record.id_string();
        let account = sync.account.clone();
        state
            .by_href
            .insert((account.clone(), entry.href.clone()), record.clone());
        state
            .by_uid
            .insert((account.clone(), entry.uid.clone()), record.clone());
        state.events.insert(
            record,
            Ev {
                account,
                calendar: entry.calendar.clone(),
                href: entry.href.clone(),
                confirmed: entry.clone(),
            },
        );
    }
    for record in &removed {
        state.forget(record);
    }
    Ok(true)
}

/// Whether a write waits for the entry `uid` of `account`: what a server says of it was read
/// before the change was told, so it is no news of it.
fn waiting(state: &State, pending: &BTreeMap<Request, Pending>, account: &str, uid: &str) -> bool {
    pending.values().any(|waiting| match waiting {
        Pending::Put { record, .. } | Pending::Remove { record, .. } => state
            .events
            .get(record)
            .is_some_and(|ev| ev.account == account && ev.confirmed.uid == uid),
        Pending::Asked(_) => false,
    })
}

fn fields_differ(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    fields: &[(String, Value)],
) -> bool {
    fields.iter().any(|(id, value)| {
        let name = ty
            .fields
            .iter()
            .find(|(_, field)| field.as_str() == id.as_str())
            .map(|(name, _)| name.as_str());
        match name {
            Some(name) => try_read(host, at, ty, name).ok().flatten().as_ref() != Some(value),
            None => true,
        }
    })
}

fn text(s: &str) -> Value {
    Value::text(s)
}

/// The field `name` when the type declares it, set to `value`, among `fields`.
fn put(ty: &OriginType, fields: &mut Vec<(String, Value)>, name: &str, value: Value) {
    if let Some(id) = ty.field(name) {
        fields.push((id.to_owned(), value));
    }
}

fn try_read(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    name: &str,
) -> Result<Option<Value>, String> {
    let Some(field) = ty.field(name) else {
        return Ok(None);
    };
    host.runtime()
        .read(at, field)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn read_text(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> String {
    try_read_text(host, at, ty, name).unwrap_or_default()
}

fn try_read_text(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    name: &str,
) -> Result<String, String> {
    Ok(match try_read(host, at, ty, name)? {
        Some(Value::Text(t)) => t,
        _ => String::new(),
    })
}

fn try_read_bool(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    name: &str,
) -> Result<bool, String> {
    Ok(match try_read(host, at, ty, name)? {
        Some(Value::Bool(b)) => b,
        Some(Value::Int(i)) => i != 0,
        _ => false,
    })
}

fn try_read_int(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> Result<i64, String> {
    Ok(match try_read(host, at, ty, name)? {
        Some(Value::Int(i)) => i,
        Some(Value::Text(t)) => t.parse().unwrap_or_default(),
        _ => 0,
    })
}

fn try_items(host: &Host, at: &ResourceRef, field: &str) -> Result<Vec<ResourceRef>, String> {
    Ok(
        match host
            .runtime()
            .read(at, field)
            .map_err(|error| error.to_string())?
        {
            Value::List(items) => items
                .into_iter()
                .filter_map(|v| match v {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
    )
}

fn try_texts(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    name: &str,
) -> Result<Vec<String>, String> {
    Ok(match try_read(host, at, ty, name)? {
        Some(Value::List(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::Text(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// What an account is identified by, without the case and spaces addresses are written with.
fn identity(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The entry `at` says now.
fn try_entry(host: &Host, at: &ResourceRef, ty: &OriginType) -> Result<Entry, String> {
    Ok(Entry {
        uid: try_read_text(host, at, ty, "key")?,
        href: try_read_text(host, at, ty, "href")?,
        calendar: try_read_text(host, at, ty, "calendar")?,
        title: try_read_text(host, at, ty, "title")?,
        start: try_read_text(host, at, ty, "start")?,
        end: try_read_text(host, at, ty, "end")?,
        all_day: try_read_bool(host, at, ty, "all_day")?,
        timezone: try_read_text(host, at, ty, "timezone")?,
        location: try_read_text(host, at, ty, "location")?,
        repeats: try_read_text(host, at, ty, "repeats")?,
        repeat_every: u32::try_from(try_read_int(host, at, ty, "repeat_every")?).unwrap_or(1),
        repeat_until: try_read_text(host, at, ty, "repeat_until")?,
        repeat_count: u32::try_from(try_read_int(host, at, ty, "repeat_count")?).unwrap_or(0),
        repeat_exdates: try_texts(host, at, ty, "repeat_exdates")?,
        overrides: json_lines(&try_read_text(host, at, ty, "overrides")?),
        organizer: try_read_text(host, at, ty, "organizer")?,
        attendees: json_lines(&try_read_text(host, at, ty, "attendees")?),
        categories: try_texts(host, at, ty, "categories")?,
        notes: try_read_text(host, at, ty, "notes")?,
    })
}

fn json_lines<T: serde::de::DeserializeOwned + Default>(text: &str) -> Vec<T> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn json_of<T: serde::Serialize>(items: &[T]) -> String {
    items
        .iter()
        .filter_map(|item| serde_json::to_string(item).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the record of the entry `entry` says, every field of it.
fn entry_fields(ty: &OriginType, entry: &Entry) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "key", text(&entry.uid));
    put(ty, &mut fields, "href", text(&entry.href));
    put(ty, &mut fields, "calendar", text(&entry.calendar));
    put(ty, &mut fields, "title", text(&entry.title));
    put(ty, &mut fields, "start", text(&entry.start));
    put(ty, &mut fields, "end", text(&entry.end));
    put(ty, &mut fields, "all_day", Value::Bool(entry.all_day));
    put(ty, &mut fields, "timezone", text(&entry.timezone));
    put(ty, &mut fields, "location", text(&entry.location));
    put(ty, &mut fields, "repeats", text(&entry.repeats));
    put(
        ty,
        &mut fields,
        "repeat_every",
        Value::Int(i64::from(entry.repeat_every)),
    );
    put(ty, &mut fields, "repeat_until", text(&entry.repeat_until));
    put(
        ty,
        &mut fields,
        "repeat_count",
        Value::Int(i64::from(entry.repeat_count)),
    );
    put(
        ty,
        &mut fields,
        "repeat_exdates",
        Value::List(entry.repeat_exdates.iter().map(|date| text(date)).collect()),
    );
    put(
        ty,
        &mut fields,
        "overrides",
        text(&json_of(&entry.overrides)),
    );
    put(ty, &mut fields, "organizer", text(&entry.organizer));
    put(
        ty,
        &mut fields,
        "attendees",
        text(&json_of(&entry.attendees)),
    );
    put(
        ty,
        &mut fields,
        "categories",
        Value::List(entry.categories.iter().map(|name| text(name)).collect()),
    );
    put(ty, &mut fields, "notes", text(&entry.notes));
    fields
}

fn calendar_fields(ty: &OriginType, cal: &CalendarRef) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "name", text(&cal.name));
    put(ty, &mut fields, "account", text(&cal.account));
    put(ty, &mut fields, "color", text(&cal.color));
    put(ty, &mut fields, "writable", Value::Bool(cal.writable));
    fields
}

/// Whether two entries differ in anything but where their server keeps them.
fn same_entry(a: &Entry, b: &Entry) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a.href.clear();
    b.href.clear();
    a == b
}

fn types_of(host: &Host, app: &str) -> Option<(Types, String)> {
    let package = host.package(app)?;
    let found = origin_types(&package.meta, "caldav");
    let kind = |k: &str| found.iter().find(|t| t.kind == k).cloned();
    let account = kind("caldav.account")?;
    let calendar = kind("caldav.calendar")?;
    let event = kind("caldav.event")?;
    let (_, root_ty) = package.main()?;
    let root = package.meta.types.iter().find(|t| t.id == root_ty)?;
    let name_of = |ty: &OriginType| {
        package
            .meta
            .types
            .iter()
            .find(|t| t.id == ty.ty)
            .map(|t| t.name.clone())
    };
    let collection = |ty: &OriginType| {
        let want = format!("Collection<{}>", name_of(ty)?);
        root.fields
            .iter()
            .find(|f| f.ty == want)
            .map(|f| f.id.clone())
    };
    Some((
        Types {
            accounts_field: collection(&account)?,
            calendars_field: collection(&calendar)?,
            events_field: collection(&event)?,
            account,
            calendar,
            event,
        },
        root_ty,
    ))
}

impl CalDavOrigin {
    /// The workspace's records read once: what was taken in before stays as it is.
    fn load(
        host: &Host,
        types: Types,
        root: ResourceRef,
        known: BTreeMap<String, ResourceRef>,
    ) -> Result<State, String> {
        let mut records = known;
        let mut state = State {
            types,
            root,
            accounts: HashMap::new(),
            calendars: HashMap::new(),
            events: HashMap::new(),
            by_href: HashMap::new(),
            by_uid: HashMap::new(),
        };
        let t = state.types.clone();
        for record in try_items(host, &state.root, &t.accounts_field)? {
            let key = try_read_text(host, &record, &t.account, "key")?;
            records.insert(key, record);
        }
        for (key, record) in records {
            let read_only = try_read_bool(host, &record, &t.account, "read_only")?;
            let mut acct = Acct {
                record,
                calendars: HashMap::new(),
                read_only,
            };
            if let Some(field) = t.account.field("calendars") {
                for calendar in try_items(host, &acct.record, field)? {
                    let ckey = try_read_text(host, &calendar, &t.calendar, "key")?;
                    acct.calendars.insert(ckey, calendar);
                }
            }
            state.accounts.insert(key, acct);
        }
        for calendar in try_items(host, &state.root, &t.calendars_field)? {
            let key = try_read_text(host, &calendar, &t.calendar, "key")?;
            let account = try_read_text(host, &calendar, &t.calendar, "account")?;
            if !account.is_empty() {
                state.calendars.insert(key, account);
            }
        }
        for at in try_items(host, &state.root, &t.events_field)? {
            let entry = try_entry(host, &at, &t.event)?;
            if entry.href.is_empty() {
                // An entry the origin has not written to a server: the workspace's own.
                continue;
            }
            let Some(account) = state.calendar_of(&entry.calendar).map(str::to_owned) else {
                continue;
            };
            let record = at.id_string();
            state
                .by_href
                .insert((account.clone(), entry.href.clone()), record.clone());
            state
                .by_uid
                .insert((account.clone(), entry.uid.clone()), record.clone());
            state.events.insert(
                record,
                Ev {
                    account,
                    calendar: entry.calendar.clone(),
                    href: entry.href.clone(),
                    confirmed: entry,
                },
            );
        }
        Ok(state)
    }
}

impl Origin for CalDavOrigin {
    fn name(&self) -> &'static str {
        "caldav"
    }

    fn serves(&self, capability: &str) -> bool {
        matches!(
            capability,
            "caldav.check" | "caldav.answer" | "caldav.import"
        )
    }

    fn due(&self) -> bool {
        self.state.is_none()
            || self.accounts_waiting
            || self.woken.load(Ordering::Acquire)
            || self.reconnected.load(Ordering::Acquire)
            || !self.due.is_empty()
            || !self.changed.borrow().is_empty()
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let _polling = Polling::start(&self.polling);
        self.woken.store(false, Ordering::Release);
        let mut polled = Polled::default();
        if self.state.is_none() || self.reload {
            let Some((types, _)) = types_of(cx.host, cx.app) else {
                return polled;
            };
            let Ok(root) = cx.host.root_of(cx.app) else {
                return polled;
            };
            let known = self.state.as_ref().map_or_else(BTreeMap::new, |state| {
                state
                    .accounts
                    .iter()
                    .map(|(key, acct)| (key.clone(), acct.record.clone()))
                    .collect()
            });
            let state = match Self::load(cx.host, types, root.clone(), known) {
                Ok(state) => state,
                Err(error) => {
                    tracing::warn!(%error, "the calendar accounts could not be read");
                    if self.state.is_some() {
                        polled.wrote |= self.write_account_state(cx.host, &cx.now);
                    }
                    return polled;
                }
            };
            if !self.subscribed {
                let changed = Rc::clone(&self.changed);
                let polling = Rc::clone(&self.polling);
                let mut store = cx.host.store().clone();
                store.subscribe(
                    &root.doc,
                    Box::new(move |summary| {
                        if !polling.get() {
                            changed.borrow_mut().extend(summary.fields.iter().cloned());
                        }
                    }),
                );
                self.subscribed = true;
            }
            self.state = Some(state);
            self.reload = false;
        }
        polled.wrote |= self.push(cx.host, &cx.now);
        polled.wrote |= self.take_again(cx.host, &cx.now);
        let events = self.take_events(cx.host, &cx.now);
        polled.wrote |= events.wrote;
        polled.settle |= events.settle;
        polled.wrote |= self.moved(cx.host, &cx.now);
        polled.wrote |= self.add_accounts(cx.host, &cx.now);
        if self.reload {
            polled.wrote |= self.write_account_state(cx.host, &cx.now);
            return polled;
        }
        polled.settle |= self.reconnect(cx.host);
        if let Some(state) = self.state.as_ref() {
            for account in due_syncs(state, &self.asked) {
                self.due.push(account);
            }
        }
        for account in std::mem::take(&mut self.due) {
            if self.asked.insert(account.clone()) && self.connector.sync(&account).is_none() {
                tracing::warn!(
                    account,
                    "the calendar connector took no request for the account"
                );
            }
        }
        polled.wrote |= self.write_account_state(cx.host, &cx.now);
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        if capability == "caldav.check" {
            return self.check_now(payload.as_str().unwrap_or_default());
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
        let known = self
            .state
            .as_ref()
            .is_some_and(|state| state.accounts.contains_key(&account));
        if !known {
            return Delivery::Rejected(format!("no calendar account {account} on this device"));
        }
        if self
            .state
            .as_ref()
            .and_then(|state| state.accounts.get(&account))
            .is_some_and(|acct| acct.read_only)
        {
            return Delivery::Rejected(READ_ONLY.to_owned());
        }
        let asked = match capability {
            "caldav.answer" => {
                let uid = field(1);
                let Some(held) = self
                    .state
                    .as_ref()
                    .and_then(|state| state.record_of(&account, &uid))
                    .and_then(|record| self.state.as_ref()?.events.get(&record).cloned())
                else {
                    return Delivery::Rejected(
                        "the calendar holds no event of that account under that name".to_owned(),
                    );
                };
                self.connector
                    .answer(&account, &held.calendar, &held.href, &field(2), &field(3))
            }
            "caldav.import" => {
                let calendar = field(1);
                let owned = self
                    .state
                    .as_ref()
                    .and_then(|state| state.calendar_of(&calendar).map(str::to_owned));
                if owned.as_deref() != Some(account.as_str()) {
                    return Delivery::Rejected(
                        "That calendar is not one of the account's".to_owned(),
                    );
                }
                self.connector.import(&account, &calendar, &field(2))
            }
            other => {
                return Delivery::Rejected(format!(
                    "the calendar connector does not carry out {other}"
                ));
            }
        };
        match asked {
            Ok(request) => {
                self.pending.insert(request, Pending::Asked(key.clone()));
                self.sends.insert(key, Sending::Going);
                Delivery::Pending
            }
            Err(Unmade::NoAccount) => {
                Delivery::Rejected(format!("no calendar account {account} on this device"))
            }
            Err(Unmade::EngineStopped) => {
                Delivery::Offline("the calendar engine stopped".to_owned())
            }
        }
    }
}
