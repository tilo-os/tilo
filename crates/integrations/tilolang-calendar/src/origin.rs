//! The `CalDAV` origin: keeps a package's types of the `caldav` kinds (`type Event (origin:
//! caldav.event)`) in step with the servers of the workspace's calendar accounts, through the
//! connector (`super::connector`), and carries out `caldav.check`, `caldav.answer` and
//! `caldav.import`.
//!
//! - What a server holds is written into the package's records as changes of the origin: an
//!   account's calendars, their entries, the people each names and the occurrences that stand
//!   apart, keyed by the ids the server gives them so nothing appears twice.
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

use super::connector::{CalDavConnector, Entry, Event, Guest, Occurrence, Sync, Unmade};

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
    "notes",
    "categories",
    "recurrence",
    "recurrence_every",
    "recurrence_until",
    "recurrence_count",
    "exdates",
    "calendar",
    "color",
];

/// The fields of a calendar people change.
const CALENDAR_PEOPLE: &[&str] = &["name", "color"];

#[derive(Clone)]
struct Types {
    /// The root's collection of calendars.
    calendars_field: String,
    /// The root's collection of entries.
    events_field: String,
    /// The root's collection of accounts, where the package publishes one.
    accounts_field: Option<String>,
    calendar: OriginType,
    event: OriginType,
    attendee: OriginType,
    /// The account type, where the package publishes one: what the account's own record says
    /// of how its server answers.
    account: Option<OriginType>,
}

#[derive(Clone)]
struct Cal {
    /// The address its server keeps it at.
    key: String,
    account: String,
    /// Where its last pull ended, as its server names it.
    token: String,
    /// What its server said of it when it was last read or written.
    name: String,
    color: String,
}

#[derive(Clone)]
struct Ev {
    account: String,
    /// The calendar's key.
    calendar: String,
    href: String,
    /// What the record said when it was last written: what the server holds, as far as it was
    /// told.
    confirmed: Entry,
    /// The record of the repeating entry this is an occurrence of, where it is one.
    master: Option<String>,
    /// The moment this occurrence would have started, where it is one.
    original_start: String,
}

struct State {
    types: Types,
    root: ResourceRef,
    /// Whether each account is connected for reading only.
    accounts: HashMap<String, bool>,
    /// The record each account says its own state in, where the package publishes accounts.
    account_records: HashMap<String, ResourceRef>,
    /// Each calendar of an account's server, by its record.
    calendars: HashMap<String, Cal>,
    /// The record of each calendar, by its account and the address its server keeps it at.
    by_key: HashMap<(String, String), String>,
    /// Each entry the origin took in, by its record.
    events: HashMap<String, Ev>,
    /// The record of each entry of each account, by the account and the href of its entry.
    by_href: HashMap<(String, String), String>,
    /// The record of each repeating entry of each account, by the account and its `UID`.
    by_uid: HashMap<(String, String), String>,
    /// The records naming the people of each entry, by its record and their address.
    attendees: HashMap<String, HashMap<String, String>>,
}

impl State {
    /// The record of the repeating entry `uid` of `account`.
    fn record_of(&self, account: &str, uid: &str) -> Option<String> {
        self.by_uid
            .get(&(account.to_owned(), uid.to_owned()))
            .cloned()
    }

    /// The record of the calendar `account`'s server keeps at `key`.
    fn calendar_record(&self, account: &str, key: &str) -> Option<String> {
        self.by_key
            .get(&(account.to_owned(), key.to_owned()))
            .cloned()
    }

    /// Whether `calendar` is an account's, and which.
    fn calendar_of(&self, calendar: &str) -> Option<&str> {
        let cal = self.calendars.get(calendar)?;
        (!cal.account.is_empty()).then_some(cal.account.as_str())
    }

    /// The account whose server keeps the calendar at `key`.
    fn calendar_ptr(&self, key: &str) -> Option<&str> {
        self.calendars
            .values()
            .find(|cal| cal.key == key)
            .map(|cal| cal.account.as_str())
    }

    fn forget(&mut self, record: &str) {
        if let Some(ev) = self.events.remove(record) {
            self.by_href.remove(&(ev.account, ev.href));
        }
        self.attendees.remove(record);
    }
}

/// A request handed to a server and not answered yet.
enum Pending {
    /// An entry written; `before` is what the record said of the server before it was changed.
    Put { record: String, before: Entry },
    /// An entry taken out, by its record: it moves to the workspace's own calendar.
    Remove { record: String, before: Entry },
    /// A calendar's own properties written.
    Calendar {
        record: String,
        name: String,
        color: String,
    },
    /// An effect of a capability, by the payload that asked for it: the account to ask again
    /// once it is taken, and the person whose answer the entry says now, where it is one.
    Asked {
        key: String,
        account: String,
        answer: Option<(String, String, String)>,
    },
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
        entry: Box<Entry>,
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
    /// What changed in the room since the last poll, as the store told it.
    changed: Rc<RefCell<Vec<FieldKey>>>,
    subscribed: bool,
    /// Set while the origin polls: what the store tells then is its own writing.
    polling: Rc<std::cell::Cell<bool>>,
    pending: BTreeMap<uuid::Uuid, Pending>,
    /// The outcome of each `caldav.answer` or `caldav.import`, by the payload that asked.
    sends: HashMap<String, Sending>,
    /// Why each account's server could not be reached, until it can.
    trouble: HashMap<String, String>,
    /// Why each account's calendars could not be written into the workspace, until they can be.
    unsaved: HashMap<String, String>,
    /// When each account's calendars were last taken in: what the account says of itself.
    fetched: HashMap<String, String>,
    /// Why the last write of each record was refused, until it is written again.
    refused: HashMap<String, String>,
    /// The steps the workspace could not take, oldest first.
    unwritten: Unwritten<Step>,
    /// The accounts whose changes to take in.
    due: Vec<String>,
    /// The accounts a sync was asked for, so one that fails is not asked again every poll.
    asked: HashSet<String>,
    /// The accounts whose server is being asked right now, until it answers.
    checking: HashSet<String>,
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
            checking: HashSet::new(),
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
                    self.checking.remove(&sync.account);
                    self.fetched.insert(sync.account.clone(), now.to_owned());
                    self.trouble.remove(&sync.account);
                    polled.wrote |= self.take_sync(host, now, sync).wrote();
                }
                Event::Unreachable { account, reason } => {
                    self.checking.remove(&account);
                    self.trouble.insert(account, reason);
                }
                Event::Accepted { request, href } => {
                    match self.pending.remove(&request) {
                        Some(Pending::Put { record, .. }) => {
                            polled.wrote |= self.stored(host, now, &record, &href);
                        }
                        Some(Pending::Remove { record, .. }) => {
                            polled.wrote |= self.taken_out(host, now, &record);
                        }
                        Some(Pending::Calendar {
                            record,
                            name,
                            color,
                        }) => {
                            self.refused.remove(&record);
                            if let Some(state) = self.state.as_mut()
                                && let Some(cal) = state.calendars.get_mut(&record)
                            {
                                cal.name = name;
                                cal.color = color;
                            }
                        }
                        Some(Pending::Asked {
                            key,
                            account,
                            answer,
                        }) => {
                            if let Some((record, email, status)) = answer {
                                polled.wrote |=
                                    self.wrote_status(host, now, &record, &email, &status);
                            }
                            // What the server took is taken in: the entry it holds now, or the
                            // invitation it put in.
                            self.asked.remove(&account);
                            self.due.push(account);
                            self.sends.insert(key, Sending::Gone);
                            polled.settle = true;
                        }
                        None => {}
                    }
                }
                Event::Refused { request, reason } => {
                    polled.wrote |= self.answer_failed(host, now, request, &reason);
                    polled.settle |= self.settled_ask(&request, Sending::Refused(reason));
                }
                Event::Unsent { request, reason } => {
                    polled.wrote |= self.answer_failed(host, now, request, &reason);
                    polled.settle |= self.settled_ask(&request, Sending::Unsent(reason));
                }
            }
        }
        polled
    }

    /// A write the server refused, or could not be reached for: what the record said goes back,
    /// and the account says why.
    fn answer_failed(&mut self, host: &Host, now: &str, request: uuid::Uuid, reason: &str) -> bool {
        match self.pending.remove(&request) {
            Some(Pending::Put { record, before } | Pending::Remove { record, before }) => {
                self.refused(host, now, &record, &before, reason)
            }
            Some(Pending::Calendar { record, .. }) => {
                self.refused_calendar(host, now, &record, reason);
                true
            }
            Some(Pending::Asked { .. }) | None => false,
        }
    }

    /// The person's own answer, written into the record that names them.
    fn wrote_status(
        &self,
        host: &Host,
        now: &str,
        record: &str,
        email: &str,
        status: &str,
    ) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let t = state.types.clone();
        // The record its collection holds now, found by the address it names.
        let Some(field) = t.event.field("attendees") else {
            return false;
        };
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let Some(guest) = try_items(host, &at, field)
            .unwrap_or_default()
            .into_iter()
            .find(|guest| {
                try_read_text(host, guest, &t.attendee, "email").is_ok_and(|held| held == email)
            })
        else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.attendee, &mut fields, "status", text(status));
        let changed = host.runtime().apply_origin(
            "caldav",
            vec![OriginWrite::Set {
                target: guest,
                fields,
            }],
            Some(now.to_owned()),
        );
        changed.is_ok()
    }

    /// Whether the request was a capability's, and what became of it was kept for it.
    fn settled_ask(&mut self, request: &uuid::Uuid, outcome: Sending) -> bool {
        let Some(Pending::Asked { key, .. }) = self.pending.get(request) else {
            return false;
        };
        let key = key.clone();
        self.pending.remove(request);
        self.sends.insert(key, outcome);
        true
    }

    /// An entry the server took: its record says where it is now.
    fn stored(&mut self, host: &Host, now: &str, record: &str, href: &str) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let Some(uid) = state.events.get(record).map(|ev| ev.confirmed.uid.clone()) else {
            return false;
        };
        let t = state.types.clone();
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let Some(ev) = state.events.get_mut(record) else {
            return false;
        };
        let was = std::mem::replace(&mut ev.href, href.to_owned());
        href.clone_into(&mut ev.confirmed.href);
        let account = ev.account.clone();
        state.by_href.remove(&(account.clone(), was));
        state
            .by_href
            .insert((account.clone(), href.to_owned()), record.to_owned());
        state
            .by_uid
            .insert((account.clone(), uid.clone()), record.to_owned());
        self.refused.remove(record);
        self.settled(&account);
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.event, &mut fields, "key", text(&uid));
        put(&t.event, &mut fields, "href", text(href));
        host.runtime()
            .apply_origin(
                "caldav",
                vec![OriginWrite::Set { target: at, fields }],
                Some(now.to_owned()),
            )
            .is_ok()
    }

    /// An entry the server took out: it is no longer on the server, and its record says so,
    /// keeping every field the person gave it.
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
        let t = state.types.clone();
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.event, &mut fields, "href", text(""));
        host.runtime()
            .apply_origin(
                "caldav",
                vec![OriginWrite::Set { target: at, fields }],
                Some(now.to_owned()),
            )
            .is_ok()
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
        let calendar = state.calendar_record(&account, &before.calendar);
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        let writes = vec![OriginWrite::Set {
            target: at,
            fields: entry_fields(&t.event, before, calendar.as_deref()),
        }];
        let taken = self.put_step(host, now, &account, writes);
        if !taken.done() {
            self.unwritten.keep(Step::Restore {
                account,
                record: record.to_owned(),
                entry: Box::new(before.clone()),
            });
        }
        taken.wrote()
    }

    /// The server refused a calendar's own properties: its record says what the server holds.
    fn refused_calendar(&mut self, host: &Host, now: &str, record: &str, reason: &str) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let Some(cal) = state.calendars.get(record) else {
            return;
        };
        let account = cal.account.clone();
        let fields = calendar_fields(&state.types.calendar, &cal.name, &cal.color);
        let Some(at) = ResourceRef::parse_id(record, &state.types.calendar.ty) else {
            return;
        };
        self.unsaved.insert(account.clone(), reason.to_owned());
        self.refused.insert(record.to_owned(), reason.to_owned());
        let _ = self.put_step(
            host,
            now,
            &account,
            vec![OriginWrite::Set { target: at, fields }],
        );
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
                tracing::warn!(
                    account = %sync.account,
                    %error,
                    "what a calendar server said could not be written"
                );
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
                    let calendar = state.calendar_record(account, &entry.calendar);
                    let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
                        return false;
                    };
                    let account = account.clone();
                    let entry = entry.clone();
                    self.put_step(
                        host,
                        now,
                        &account,
                        vec![OriginWrite::Set {
                            target: at,
                            fields: entry_fields(&t.event, &entry, calendar.as_deref()),
                        }],
                    )
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
        let mut events: BTreeSet<String> = BTreeSet::new();
        let mut calendars: BTreeSet<String> = BTreeSet::new();
        {
            let Some(state) = self.state.as_ref() else {
                return false;
            };
            let t = &state.types;
            let people: BTreeSet<&str> = PEOPLE
                .iter()
                .filter_map(|name| t.event.field(name))
                .collect();
            let named: BTreeSet<&str> = CALENDAR_PEOPLE
                .iter()
                .filter_map(|name| t.calendar.field(name))
                .collect();
            for key in keys {
                if key.field == "*" {
                    events.insert(key.resource.clone());
                    calendars.insert(key.resource);
                    continue;
                }
                if people.contains(key.field.as_str()) {
                    events.insert(key.resource.clone());
                }
                if named.contains(key.field.as_str()) {
                    calendars.insert(key.resource);
                }
            }
        }
        let mut wrote = false;
        for record in events {
            wrote |= self.pushed(host, now, &record);
        }
        for record in calendars {
            wrote |= self.pushed_calendar(host, now, &record);
        }
        wrote
    }

    /// What one calendar's record says now, handed to its server.
    fn pushed_calendar(&mut self, host: &Host, now: &str, record: &str) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        let Some(cal) = state.calendars.get(record) else {
            return false;
        };
        if cal.account.is_empty() || state.accounts.get(&cal.account).copied() != Some(false) {
            return false;
        }
        let t = state.types.clone();
        let Some(at) = ResourceRef::parse_id(record, &t.calendar.ty) else {
            return false;
        };
        let name = read_text(host, &at, &t.calendar, "name");
        let color = read_text(host, &at, &t.calendar, "color");
        if name == cal.name && color == cal.color {
            return false;
        }
        let account = cal.account.clone();
        let key = cal.key.clone();
        match self.connector.set_calendar(&account, &key, &name, &color) {
            Ok(request) => {
                self.pending.insert(
                    request,
                    Pending::Calendar {
                        record: record.to_owned(),
                        name,
                        color,
                    },
                );
                true
            }
            Err(Unmade::NoAccount) => {
                let reason = "no calendar account on this device is that one";
                self.unsaved.insert(account, reason.to_owned());
                self.refused_calendar(host, now, record, reason);
                true
            }
            Err(Unmade::EngineStopped) => {
                self.unsaved
                    .insert(account, "The calendar engine stopped".to_owned());
                true
            }
        }
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
        // An occurrence that stands apart is written with its series.
        if let Some(master) = state.events.get(record).and_then(|ev| ev.master.clone()) {
            return self.pushed(host, now, &master);
        }
        let calendar = match try_read_ref(host, &at, &t.event, "calendar") {
            Ok(Some(calendar)) => calendar.id_string(),
            // The record is gone, or so is its calendar: the entry goes from its server where
            // it is on one.
            _ => {
                return known && self.remove_from_server(host, now, record);
            }
        };
        let Ok(current) = read_entry(host, state, &at, &t.event) else {
            return known && self.remove_from_server(host, now, record);
        };
        let held = state.events.get(record).cloned();
        let Some(target_account) = state.calendar_of(&calendar).map(str::to_owned) else {
            // The workspace's own calendar: the origin never sends it. One that was on a server
            // is taken out there, and its fields stay as the person left them.
            return known && self.remove_from_server(host, now, record);
        };
        let Some(held) = held else {
            // An entry the origin never took in: it was made here, in an account's calendar.
            let series = self.series(host, record, current);
            return self.write_to_server(host, now, record, &series, &target_account, None);
        };
        let calendar_key = state
            .calendars
            .get(&calendar)
            .map(|cal| cal.key.clone())
            .unwrap_or_default();
        let moved = held.calendar != calendar_key;
        if moved && target_account != held.account {
            if !self.remove_from_server(host, now, record) {
                return false;
            }
            self.moving.push(record.to_owned());
            return true;
        }
        let series = self.series(host, record, current);
        if !moved && same_entry(&series, &held.confirmed) {
            return false;
        }
        let from = moved.then(|| held.href.clone());
        if self.write_to_server(host, now, record, &series, &target_account, from.as_deref()) {
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
        let calendar = state.calendar_record(&account, &before.calendar);
        let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
            return false;
        };
        self.put_step(
            host,
            now,
            &account,
            vec![OriginWrite::Set {
                target: at,
                fields: entry_fields(&t.event, &before, calendar.as_deref()),
            }],
        )
        .wrote()
    }

    /// The entry `record` and the occurrences that stand apart from it, as its server holds one
    /// entry of them all.
    fn series(&self, host: &Host, record: &str, master: Entry) -> Entry {
        let Some(state) = self.state.as_ref() else {
            return master;
        };
        let mut entry = master;
        let mut overrides: Vec<Occurrence> = Vec::new();
        for (other, ev) in &state.events {
            if ev.master.as_deref() != Some(record) {
                continue;
            }
            let t = state.types.clone();
            let Some(at) = ResourceRef::parse_id(other, &t.event.ty) else {
                continue;
            };
            let Ok(edit) = read_entry(host, state, &at, &t.event) else {
                continue;
            };
            let (date, time) = wall_split(&ev.original_start);
            let cancelled = try_read_bool(host, &at, &t.event, "cancelled").unwrap_or(false);
            overrides.push(Occurrence {
                date,
                time,
                cancelled,
                start: if cancelled { String::new() } else { edit.start },
                end: if cancelled { String::new() } else { edit.end },
                title: if cancelled { String::new() } else { edit.title },
                location: if cancelled {
                    String::new()
                } else {
                    edit.location
                },
            });
        }
        entry.overrides = overrides;
        entry
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
        if state.accounts.get(account).copied().unwrap_or(true) {
            // Never asked: a read-only account refuses before any server is.
            self.unsaved
                .insert(account.to_owned(), READ_ONLY.to_owned());
            self.refused.insert(record.to_owned(), READ_ONLY.to_owned());
            return false;
        }
        let t = state.types.clone();
        let held = state.events.get(record).cloned();
        let mut entry = entry.clone();
        if entry.uid.is_empty() {
            entry.uid = uuid::Uuid::new_v4().to_string();
        }
        let href = match &held {
            Some(ev) if also_remove.is_none() => ev.href.clone(),
            Some(_) | None => String::new(),
        };
        let before = held
            .as_ref()
            .map_or_else(|| entry.clone(), |ev| ev.confirmed.clone());
        let written = self
            .connector
            .put(account, entry.clone(), &href, also_remove.unwrap_or(""));
        let Ok(request) = written else {
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
                    href,
                    confirmed: entry.clone(),
                    master: None,
                    original_start: String::new(),
                },
            );
            state
                .by_uid
                .insert((account.to_owned(), entry.uid.clone()), record.to_owned());
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
        if state.accounts.get(&account).copied().unwrap_or(true) {
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
            let Ok(Some(calendar)) = try_read_ref(host, &at, &t.event, "calendar") else {
                continue;
            };
            let calendar = calendar.id_string();
            let Some(account) = state.calendar_of(&calendar).map(str::to_owned) else {
                continue;
            };
            let Ok(master) = read_entry(host, state, &at, &t.event) else {
                continue;
            };
            let series = self.series(host, &record, master);
            wrote |= self.write_to_server(host, now, &record, &series, &account, None);
        }
        wrote
    }

    /// Where each calendar of the account's last pull ended and each entry's version, as the
    /// workspace kept them: what a connector restarted since asks from.
    fn known_of(&self, account: &str) -> Vec<super::connector::CalendarKnown> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let mut known: Vec<super::connector::CalendarKnown> = Vec::new();
        for cal in state
            .calendars
            .values()
            .filter(|cal| cal.account == account)
        {
            let mut kept = super::connector::CalendarKnown {
                calendar: cal.key.clone(),
                token: cal.token.clone(),
                entries: Vec::new(),
            };
            for ev in state
                .events
                .values()
                .filter(|ev| ev.account == account && ev.calendar == cal.key && !ev.href.is_empty())
            {
                kept.entries
                    .push((ev.href.clone(), ev.confirmed.etag.clone()));
            }
            known.push(kept);
        }
        known
    }

    /// Each account of the connector's is asked what it holds, and says of itself in a record of
    /// its own where the package publishes accounts.
    fn follow_accounts(&mut self, host: &Host, now: &str) -> bool {
        let accounts = self.connector.accounts();
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let mut asked = false;
        for account in &accounts {
            if state.accounts.contains_key(&account.id) {
                continue;
            }
            state.accounts.insert(account.id.clone(), account.read_only);
            self.asked.remove(&account.id);
            self.due.push(account.id.clone());
            asked = true;
        }
        let (Some(field), Some(ty)) = (
            state.types.accounts_field.clone(),
            state.types.account.clone(),
        ) else {
            return asked;
        };
        let root = state.root.clone();
        let missing: Vec<super::connector::AccountRef> = accounts
            .into_iter()
            .filter(|account| !state.account_records.contains_key(&account.id))
            .collect();
        if missing.is_empty() {
            return asked;
        }
        let writes: Vec<OriginWrite> = missing
            .iter()
            .map(|account| OriginWrite::Insert {
                owner: root.clone(),
                field: field.clone(),
                ty: ty.ty.clone(),
                fields: account_fields(&ty, account),
            })
            .collect();
        let made = host
            .runtime()
            .write_origin("caldav", Some(now.to_owned()), |change| {
                change.write(writes)
            });
        match made {
            Ok(records) => {
                for (account, record) in missing.into_iter().zip(records) {
                    if let Some(record) = record {
                        state.account_records.insert(account.id, record);
                        asked = true;
                    }
                }
            }
            Err(error) => {
                // The next poll makes the accounts again.
                tracing::warn!(%error, "the calendar accounts could not be written");
            }
        }
        asked
    }
}

/// What an account's own record says of it when it is first made: who it is, and that its
/// server has not been heard from yet.
fn account_fields(ty: &OriginType, account: &super::connector::AccountRef) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "key", text(&account.id));
    put(ty, &mut fields, "address", text(&account.address));
    put(ty, &mut fields, "name", text(&account.name));
    put(ty, &mut fields, "server", text(&account.server));
    put(ty, &mut fields, "read_only", Value::Bool(account.read_only));
    put(ty, &mut fields, "online", Value::Bool(false));
    fields
}

impl CalDavOrigin {
    /// Each account says whether its server answers, what is wrong with it, whether it is being
    /// asked now, and when its calendars were last taken in.
    fn account_state(&self, host: &Host) -> Vec<OriginWrite> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let Some(ty) = state.types.account.as_ref() else {
            return Vec::new();
        };
        let mut writes = Vec::new();
        for (id, record) in &state.account_records {
            let unreached = self.trouble.get(id).map_or("", String::as_str);
            // A server not reached is said before calendars that could not be written, which
            // the next poll writes again.
            let trouble = if unreached.is_empty() {
                self.unsaved.get(id).map_or("", String::as_str)
            } else {
                unreached
            };
            let online = unreached.is_empty();
            let checking = self.checking.contains(id);
            let fetched = self.fetched.get(id).map_or("", String::as_str);
            if read_text(host, record, ty, "trouble") != trouble
                || read_bool(host, record, ty, "online") != online
                || read_bool(host, record, ty, "checking") != checking
                || (!fetched.is_empty() && read_text(host, record, ty, "fetched") != fetched)
            {
                let mut fields = Vec::new();
                put(ty, &mut fields, "trouble", text(trouble));
                put(ty, &mut fields, "online", Value::Bool(online));
                put(ty, &mut fields, "checking", Value::Bool(checking));
                if !fetched.is_empty() {
                    put(ty, &mut fields, "fetched", text(fetched));
                }
                writes.push(OriginWrite::Set {
                    target: record.clone(),
                    fields,
                });
            }
        }
        writes
    }

    /// What each account says of itself is written, once for all of them.
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
                tracing::warn!(%error, "what a calendar account says of itself was not written");
                false
            }
        }
    }
}

/// The wall time a day and a time of day are (`2026-11-24T09:00:00`), as iCalendar writes one.
fn wall_at(date: &str, time: &str) -> String {
    if date.is_empty() {
        return String::new();
    }
    let time = if time.is_empty() { "00:00:00" } else { time };
    format!("{date}T{time}")
}

/// The day a wall time is written on (`2026-11-24`).
fn wall_day(wall: &str) -> String {
    wall.get(0..10).unwrap_or_default().to_owned()
}

/// The day and time of day a wall time is written at.
fn wall_split(wall: &str) -> (String, String) {
    let date = wall_day(wall);
    let time = wall
        .split_once(['T', ' '])
        .map(|(_, time)| time.to_owned())
        .unwrap_or_default();
    (date, time)
}

/// The last day the entry `entry` is on: for an all-day entry, its end is the day after it.
fn end_day(entry: &Entry) -> String {
    let end = wall_day(&entry.end);
    if !entry.all_day {
        return end;
    }
    let Ok(date) = jiff::civil::Date::strptime("%Y-%m-%d", &end) else {
        return end;
    };
    date.yesterday().map(|day| day.to_string()).unwrap_or(end)
}

/// What the record of one occurrence of `entry` says: the fields it changes, and the series it
/// is one of.
fn override_fields(
    ty: &OriginType,
    entry: &Entry,
    occurrence: &Occurrence,
    master: &ResourceRef,
    calendar: Option<&str>,
    original_start: &str,
) -> Vec<(String, Value)> {
    let mut fields = entry_fields(ty, entry, calendar);
    fields.retain(|(id, _)| {
        ty.fields.iter().any(|(name, field)| {
            field == id && (name == "key" || name == "href" || name == "timezone")
        })
    });
    let title = if occurrence.title.is_empty() {
        entry.title.clone()
    } else {
        occurrence.title.clone()
    };
    let location = if occurrence.location.is_empty() {
        entry.location.clone()
    } else {
        occurrence.location.clone()
    };
    let start = if occurrence.start.is_empty() {
        entry.start.clone()
    } else {
        occurrence.start.clone()
    };
    let end = if occurrence.end.is_empty() {
        entry.end.clone()
    } else {
        occurrence.end.clone()
    };
    put(ty, &mut fields, "title", text(&title));
    put(ty, &mut fields, "start", text(&start));
    put(ty, &mut fields, "end", text(&end));
    put(ty, &mut fields, "all_day", Value::Bool(entry.all_day));
    put(ty, &mut fields, "location", text(&location));
    put(ty, &mut fields, "notes", text(&entry.notes));
    put(ty, &mut fields, "color", text(&entry.color));
    put(ty, &mut fields, "day", text(&wall_day(&start)));
    put(ty, &mut fields, "end_day", text(&end_day(entry)));
    put(
        ty,
        &mut fields,
        "cancelled",
        Value::Bool(occurrence.cancelled),
    );
    put(ty, &mut fields, "original_start", text(original_start));
    put(
        ty,
        &mut fields,
        "recurrence_of",
        Value::Opt(Some(Box::new(Value::resource(master)))),
    );
    fields
}

/// What one sync wrote, to be taken into the state once the workspace keeps it.
struct Made {
    /// Each new calendar: the address its server keeps it at, and its record.
    calendars: Vec<(String, ResourceRef)>,
    /// Each entry written, with its record, the people it names and the occurrences that stand
    /// apart from it.
    entries: Vec<Written>,
    /// The records of occurrences no longer of any series.
    forgotten: Vec<String>,
}

struct Written {
    entry: Entry,
    record: ResourceRef,
    master: Option<String>,
    people: Vec<(String, ResourceRef)>,
    overrides: Vec<(Occurrence, ResourceRef)>,
    gone_overrides: Vec<String>,
    gone_people: Vec<String>,
}

/// What a server said changed in `account`, written as one change of the origin.
#[allow(
    clippy::too_many_lines,
    reason = "the whole of one pull, written as one change of the origin"
)]
fn write_sync(
    host: &Host,
    now: &str,
    state: &mut State,
    pending: &BTreeMap<uuid::Uuid, Pending>,
    sync: &Sync,
) -> Result<bool, RuntimeError> {
    let t = state.types.clone();
    if !state.accounts.contains_key(&sync.account) {
        return Err(RuntimeError::NotFound(format!(
            "no calendar account {} in the workspace as it was read",
            sync.account
        )));
    }
    // Calendars the server no longer lists, and the entries of each: they go.
    let listed: HashSet<&str> = sync.calendars.iter().map(|cal| cal.key.as_str()).collect();
    let gone_calendars: Vec<String> = state
        .calendars
        .iter()
        .filter(|(_, cal)| cal.account == sync.account && !listed.contains(cal.key.as_str()))
        .map(|(record, _)| record.clone())
        .collect();
    let gone_events: Vec<String> = state
        .events
        .iter()
        .filter(|(_, ev)| ev.account == sync.account && !listed.contains(ev.calendar.as_str()))
        .map(|(record, _)| record.clone())
        .collect();
    // What this pull writes: the entries new to the workspace, and the ones it brings up to date.
    let mut updates: Vec<(String, Entry)> = Vec::new();
    let mut inserts: Vec<Entry> = Vec::new();
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
    let made = host
        .runtime()
        .write_origin("caldav", Some(now.to_owned()), |change| {
            let mut made = Made {
                calendars: Vec::new(),
                entries: Vec::new(),
                forgotten: Vec::new(),
            };
            // Calendars first: the entries name them.
            let mut writes = Vec::new();
            let mut new_calendars: Vec<(String, usize)> = Vec::new();
            for cal in &sync.calendars {
                if let Some(record) = state.calendar_record(&cal.account, &cal.key) {
                    if let Some(at) = ResourceRef::parse_id(&record, &t.calendar.ty) {
                        let mut fields = calendar_fields(&t.calendar, &cal.name, &cal.color);
                        put(&t.calendar, &mut fields, "sync_token", text(&cal.token));
                        if fields_differ(host, &at, &t.calendar, &fields) {
                            writes.push(OriginWrite::Set { target: at, fields });
                        }
                    }
                    continue;
                }
                let mut fields = calendar_fields(&t.calendar, &cal.name, &cal.color);
                put(&t.calendar, &mut fields, "sync_token", text(&cal.token));
                put(&t.calendar, &mut fields, "key", text(&cal.key));
                put(&t.calendar, &mut fields, "account", text(&cal.account));
                put(
                    &t.calendar,
                    &mut fields,
                    "writable",
                    Value::Bool(cal.writable),
                );
                new_calendars.push((cal.key.clone(), writes.len()));
                writes.push(OriginWrite::Insert {
                    owner: root.clone(),
                    field: t.calendars_field.clone(),
                    ty: t.calendar.ty.clone(),
                    fields,
                });
            }
            let results = change.write(writes)?;
            for (key, slot) in new_calendars {
                if let Some(Some(record)) = results.get(slot) {
                    made.calendars.push((key, record.clone()));
                }
            }
            // The calendars this pull made, by the address their server keeps them at: the
            // entries of them name the records they are inserted as.
            let made_calendars: HashMap<String, String> = made
                .calendars
                .iter()
                .map(|(key, record)| (key.clone(), record.id_string()))
                .collect();
            let calendar_record = |key: &str| {
                state
                    .calendar_record(&sync.account, key)
                    .or_else(|| made_calendars.get(key).cloned())
            };

            let mut writes = Vec::new();
            for record in gone_calendars.iter().chain(&gone_events).chain(&removed) {
                if let Some(record) = state.forgotten_by(record) {
                    writes.push(record);
                }
            }
            change.write(writes)?;

            let mut updates_writes = Vec::new();
            for (record, entry) in &updates {
                let Some(at) = ResourceRef::parse_id(record, &t.event.ty) else {
                    continue;
                };
                let fields =
                    entry_fields(&t.event, entry, calendar_record(&entry.calendar).as_deref());
                updates_writes.push(OriginWrite::Set {
                    target: at.clone(),
                    fields,
                });
                made.entries.push(Written {
                    entry: entry.clone(),
                    record: at,
                    master: None,
                    people: Vec::new(),
                    overrides: Vec::new(),
                    gone_overrides: Vec::new(),
                    gone_people: Vec::new(),
                });
            }
            let mut insert_writes = Vec::new();
            let mut insert_slots: Vec<usize> = Vec::new();
            for entry in &inserts {
                insert_slots.push(updates_writes.len().saturating_add(insert_writes.len()));
                insert_writes.push(OriginWrite::Insert {
                    owner: root.clone(),
                    field: t.events_field.clone(),
                    ty: t.event.ty.clone(),
                    fields: entry_fields(
                        &t.event,
                        entry,
                        calendar_record(&entry.calendar).as_deref(),
                    ),
                });
            }
            let combined = std::mem::take(&mut updates_writes);
            let update_results = change.write(combined)?;
            let insert_results = change.write(insert_writes)?;
            for (index, slot) in insert_slots.into_iter().enumerate() {
                let _ = slot;
                if let Some(Some(record)) = insert_results.get(index)
                    && let Some(entry) = inserts.get(index)
                {
                    made.entries.push(Written {
                        entry: entry.clone(),
                        record: record.clone(),
                        master: None,
                        people: Vec::new(),
                        overrides: Vec::new(),
                        gone_overrides: Vec::new(),
                        gone_people: Vec::new(),
                    });
                }
            }
            let _ = update_results;

            // The people each entry names, and the occurrences that stand apart from it.
            for written in &mut made.entries {
                let entry = written.entry.clone();
                let record = written.record.clone();
                // What the record's collection holds now, by the address each names: the
                // record itself says it, not what an earlier write left in memory.
                let mut existing: HashMap<String, String> = HashMap::new();
                if let Some(field) = t.event.field("attendees") {
                    for guest in try_items(host, &record, field).unwrap_or_default() {
                        if let Ok(email) = try_read_text(host, &guest, &t.attendee, "email") {
                            existing.insert(email, guest.id_string());
                        }
                    }
                }
                let mut wants = Vec::new();
                for guest in &entry.attendees {
                    wants.push(guest.clone());
                }
                let mut writes = Vec::new();
                for guest in &wants {
                    if let Some(at) = existing
                        .get(&guest.email)
                        .and_then(|record| ResourceRef::parse_id(record, &t.attendee.ty))
                    {
                        let fields = guest_fields(&t.attendee, guest);
                        if fields_differ(host, &at, &t.attendee, &fields) {
                            writes.push(OriginWrite::Set { target: at, fields });
                        }
                    } else {
                        writes.push(OriginWrite::Insert {
                            owner: record.clone(),
                            field: t.event.field("attendees").unwrap_or_default().to_owned(),
                            ty: t.attendee.ty.clone(),
                            fields: guest_fields(&t.attendee, guest),
                        });
                    }
                }
                for (email, at) in &existing {
                    if !wants.iter().any(|guest| &guest.email == email) {
                        if let Some(at) = ResourceRef::parse_id(at, &t.attendee.ty) {
                            writes.push(OriginWrite::Remove {
                                owner: record.clone(),
                                field: t.event.field("attendees").unwrap_or_default().to_owned(),
                                item: at,
                            });
                        }
                        written.gone_people.push(email.clone());
                    }
                }
                for guest in &wants {
                    for (email, at) in &existing {
                        if email == &guest.email
                            && !written.people.iter().any(|(held, _)| held == email)
                            && let Some(at) = ResourceRef::parse_id(at, &t.attendee.ty)
                        {
                            written.people.push((email.clone(), at));
                        }
                    }
                }
                let results = change.write(writes)?;
                for (guest, result) in wants.iter().zip(results) {
                    if let Some(record) = result {
                        written.people.push((guest.email.clone(), record));
                    }
                }
            }

            // The occurrences that stand apart from each entry: one record each, naming the
            // series it is one of.
            for written in &mut made.entries {
                let entry = written.entry.clone();
                let record = written.record.clone();
                let master = record.id_string();
                let existing: Vec<(String, String)> = state
                    .events
                    .iter()
                    .filter(|(_, ev)| ev.master.as_deref() == Some(master.as_str()))
                    .map(|(other, ev)| (other.clone(), ev.original_start.clone()))
                    .collect();
                let mut writes = Vec::new();
                let mut inserts = Vec::new();
                let mut wanted: Vec<String> = Vec::new();
                for occurrence in &entry.overrides {
                    let original = wall_at(&occurrence.date, &occurrence.time);
                    wanted.push(original.clone());
                    let calendar_record = calendar_record(&entry.calendar);
                    let fields = override_fields(
                        &t.event,
                        &entry,
                        occurrence,
                        &record,
                        calendar_record.as_deref(),
                        &original,
                    );
                    if let Some((other, _)) = existing.iter().find(|(_, start)| start == &original)
                        && let Some(at) = ResourceRef::parse_id(other, &t.event.ty)
                    {
                        writes.push(OriginWrite::Set { target: at, fields });
                        continue;
                    }
                    inserts.push(OriginWrite::Insert {
                        owner: root.clone(),
                        field: t.events_field.clone(),
                        ty: t.event.ty.clone(),
                        fields,
                    });
                }
                for (other, _) in &existing {
                    if !wanted.iter().any(|original| {
                        state
                            .events
                            .get(other)
                            .is_some_and(|ev| &ev.original_start == original)
                    }) {
                        if let Some(at) = ResourceRef::parse_id(other, &t.event.ty) {
                            writes.push(OriginWrite::Remove {
                                owner: root.clone(),
                                field: t.events_field.clone(),
                                item: at,
                            });
                        }
                        written.gone_overrides.push(other.clone());
                    }
                }
                change.write(writes)?;
                let results = change.write(inserts)?;
                let mut made_overrides: Vec<(Occurrence, ResourceRef)> = Vec::new();
                let mut slot = 0;
                for occurrence in &entry.overrides {
                    let original = wall_at(&occurrence.date, &occurrence.time);
                    if existing.iter().any(|(_, start)| start == &original) {
                        continue;
                    }
                    if let Some(Some(record)) = results.get(slot) {
                        made_overrides.push((occurrence.clone(), record.clone()));
                    }
                    slot = slot.saturating_add(1);
                }
                written.overrides = made_overrides;
            }
            Ok(made)
        })?;
    // The state keeps what the workspace now holds.
    for (key, record) in made.calendars {
        let account = sync.account.clone();
        state
            .by_key
            .insert((account.clone(), key.clone()), record.id_string());
        state.calendars.insert(
            record.id_string(),
            Cal {
                key,
                account,
                token: String::new(),
                name: String::new(),
                color: String::new(),
            },
        );
    }
    for cal in &sync.calendars {
        if let Some(record) = state.calendar_record(&cal.account, &cal.key)
            && let Some(held) = state.calendars.get_mut(&record)
        {
            cal.name.clone_into(&mut held.name);
            cal.color.clone_into(&mut held.color);
            cal.token.clone_into(&mut held.token);
        }
    }
    for record in gone_events.iter().chain(&removed) {
        state.forget(record);
    }
    for written in made.entries {
        let record = written.record.id_string();
        let account = sync.account.clone();
        state.by_href.insert(
            (account.clone(), written.entry.href.clone()),
            record.clone(),
        );
        state
            .by_uid
            .insert((account.clone(), written.entry.uid.clone()), record.clone());
        let people: HashMap<String, String> = written
            .people
            .into_iter()
            .map(|(email, at)| (email, at.id_string()))
            .collect();
        state.attendees.insert(record.clone(), people);
        for (occurrence, at) in written.overrides {
            let original = wall_at(&occurrence.date, &occurrence.time);
            state.events.insert(
                at.id_string(),
                Ev {
                    account: sync.account.clone(),
                    calendar: written.entry.calendar.clone(),
                    href: written.entry.href.clone(),
                    confirmed: Entry::default(),
                    master: Some(record.clone()),
                    original_start: original,
                },
            );
        }
        for other in written.gone_overrides {
            state.forget(&other);
        }
        state.events.insert(
            record,
            Ev {
                account,
                calendar: written.entry.calendar.clone(),
                href: written.entry.href.clone(),
                confirmed: written.entry,
                master: written.master,
                original_start: String::new(),
            },
        );
    }
    for record in made.forgotten {
        state.forget(&record);
    }
    Ok(true)
}

/// Whether a write waits for the entry `uid` of `account`.
fn waiting(
    state: &State,
    pending: &BTreeMap<uuid::Uuid, Pending>,
    account: &str,
    uid: &str,
) -> bool {
    pending.values().any(|waiting| match waiting {
        Pending::Put { record, .. } | Pending::Remove { record, .. } => state
            .events
            .get(record)
            .is_some_and(|ev| ev.account == account && ev.confirmed.uid == uid),
        Pending::Calendar { .. } | Pending::Asked { .. } => false,
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

/// Whether the record says `name`, false where it says nothing of it.
fn read_bool(host: &Host, at: &ResourceRef, ty: &OriginType, name: &str) -> bool {
    try_read_bool(host, at, ty, name).unwrap_or(false)
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
        Some(Value::Text(text)) => text.parse().unwrap_or_default(),
        _ => 0,
    })
}

/// The resource the field `name` names, where it names one.
fn try_read_ref(
    host: &Host,
    at: &ResourceRef,
    ty: &OriginType,
    name: &str,
) -> Result<Option<ResourceRef>, String> {
    let target = match try_read(host, at, ty, name)? {
        Some(Value::Ref(r)) => r.target,
        Some(Value::Opt(Some(held))) => match *held {
            Value::Ref(r) => r.target,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(Some((*target).clone()))
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

/// The entry `at` says now, its calendar's key aside its occurrences.
fn read_entry(
    host: &Host,
    state: &State,
    at: &ResourceRef,
    ty: &OriginType,
) -> Result<Entry, String> {
    let calendar = try_read_ref(host, at, ty, "calendar")?
        .map(|calendar| {
            state
                .calendars
                .get(&calendar.id_string())
                .map(|cal| cal.key.clone())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let mut attendees: Vec<Guest> = Vec::new();
    if let Some(field) = ty.field("attendees") {
        for guest in try_items(host, at, field)? {
            attendees.push(Guest {
                name: try_read_text(host, &guest, &state.types.attendee, "name")?,
                email: try_read_text(host, &guest, &state.types.attendee, "email")?,
                status: try_read_text(host, &guest, &state.types.attendee, "status")?,
                role: try_read_text(host, &guest, &state.types.attendee, "role")?,
            });
        }
    }
    let organizer = attendees
        .iter()
        .find(|guest| guest.role == "chair")
        .map(|guest| guest.email.clone())
        .unwrap_or_default();
    Ok(Entry {
        uid: try_read_text(host, at, ty, "key")?,
        href: try_read_text(host, at, ty, "href")?,
        etag: try_read_text(host, at, ty, "etag")?,
        calendar,
        title: try_read_text(host, at, ty, "title")?,
        start: try_read_text(host, at, ty, "start")?,
        end: try_read_text(host, at, ty, "end")?,
        all_day: try_read_bool(host, at, ty, "all_day")?,
        timezone: try_read_text(host, at, ty, "timezone")?,
        location: try_read_text(host, at, ty, "location")?,
        recurrence: try_read_text(host, at, ty, "recurrence")?,
        recurrence_every: u32::try_from(try_read_int(host, at, ty, "recurrence_every")?)
            .unwrap_or(1),
        recurrence_until: try_read_text(host, at, ty, "recurrence_until")?,
        recurrence_count: u32::try_from(try_read_int(host, at, ty, "recurrence_count")?)
            .unwrap_or(0),
        exdates: try_texts(host, at, ty, "exdates")?,
        overrides: Vec::new(),
        organizer,
        attendees,
        categories: try_texts(host, at, ty, "categories")?,
        color: try_read_text(host, at, ty, "color")?,
        notes: try_read_text(host, at, ty, "notes")?,
        remind: try_read_text(host, at, ty, "remind")?,
    })
}

/// What the record of the entry `entry` says, every field of it but where the server keeps it.
fn entry_fields(ty: &OriginType, entry: &Entry, calendar: Option<&str>) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "key", text(&entry.uid));
    put(ty, &mut fields, "href", text(&entry.href));
    put(ty, &mut fields, "etag", text(&entry.etag));
    put(ty, &mut fields, "title", text(&entry.title));
    put(ty, &mut fields, "start", text(&entry.start));
    put(ty, &mut fields, "end", text(&entry.end));
    put(ty, &mut fields, "all_day", Value::Bool(entry.all_day));
    put(ty, &mut fields, "timezone", text(&entry.timezone));
    put(ty, &mut fields, "location", text(&entry.location));
    put(ty, &mut fields, "notes", text(&entry.notes));
    put(ty, &mut fields, "recurrence", text(&entry.recurrence));
    put(
        ty,
        &mut fields,
        "recurrence_every",
        Value::Int(i64::from(entry.recurrence_every)),
    );
    put(
        ty,
        &mut fields,
        "recurrence_until",
        if entry.recurrence_until.is_empty() {
            Value::Opt(None)
        } else {
            Value::Opt(Some(Box::new(text(&entry.recurrence_until))))
        },
    );
    put(
        ty,
        &mut fields,
        "recurrence_count",
        if entry.recurrence_count == 0 {
            Value::Opt(None)
        } else {
            Value::Opt(Some(Box::new(Value::Int(i64::from(
                entry.recurrence_count,
            )))))
        },
    );
    put(
        ty,
        &mut fields,
        "exdates",
        Value::List(entry.exdates.iter().map(|date| text(date)).collect()),
    );
    put(
        ty,
        &mut fields,
        "categories",
        Value::List(entry.categories.iter().map(|name| text(name)).collect()),
    );
    put(ty, &mut fields, "color", text(&entry.color));
    put(ty, &mut fields, "organizer", text(&entry.organizer));
    if !entry.remind.is_empty() {
        put(ty, &mut fields, "remind", text(&entry.remind));
    }
    put(ty, &mut fields, "day", text(&wall_day(&entry.start)));
    put(ty, &mut fields, "end_day", text(&end_day(entry)));
    if let Some(calendar) = calendar
        && let Some(at) = ResourceRef::parse_id(calendar, &ty.ty)
    {
        put(
            ty,
            &mut fields,
            "calendar",
            Value::Opt(Some(Box::new(Value::resource(&at)))),
        );
    }
    fields
}

/// What the record of the person `guest` says.
fn guest_fields(ty: &OriginType, guest: &Guest) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "name", text(&guest.name));
    put(ty, &mut fields, "email", text(&guest.email));
    put(ty, &mut fields, "status", text(&guest.status));
    put(ty, &mut fields, "role", text(&guest.role));
    fields
}

fn calendar_fields(ty: &OriginType, name: &str, color: &str) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "name", text(name));
    put(ty, &mut fields, "color", text(color));
    fields
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
    let calendar = kind("caldav.calendar")?;
    let event = kind("caldav.event")?;
    let attendee = kind("caldav.attendee")?;
    let account = kind("caldav.account");
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
    // An account type with no collection to hold it is no account the package publishes, and
    // the origin says nothing of an account there.
    let accounts_field = account.as_ref().and_then(collection);
    let account = accounts_field.as_ref().and(account);
    Some((
        Types {
            calendars_field: collection(&calendar)?,
            events_field: collection(&event)?,
            accounts_field,
            calendar,
            event,
            attendee,
            account,
        },
        root_ty,
    ))
}

impl State {
    /// The write that takes the record `record` out of the root's events, where it is one of
    /// them.
    fn forgotten_by(&self, record: &str) -> Option<OriginWrite> {
        ResourceRef::parse_id(record, &self.types.event.ty).map(|at| OriginWrite::Remove {
            owner: self.root.clone(),
            field: self.types.events_field.clone(),
            item: at,
        })
    }
}

impl CalDavOrigin {
    /// The workspace's records read once: what was taken in before stays as it is.
    fn load(host: &Host, types: Types, root: ResourceRef) -> Result<State, String> {
        let mut state = State {
            types,
            root,
            accounts: HashMap::new(),
            account_records: HashMap::new(),
            calendars: HashMap::new(),
            by_key: HashMap::new(),
            events: HashMap::new(),
            by_href: HashMap::new(),
            by_uid: HashMap::new(),
            attendees: HashMap::new(),
        };
        let t = state.types.clone();
        if let (Some(field), Some(ty)) = (&t.accounts_field, &t.account) {
            for at in try_items(host, &state.root, field)? {
                let key = try_read_text(host, &at, ty, "key")?;
                if key.is_empty() {
                    continue;
                }
                state.account_records.insert(key, at);
            }
        }
        for at in try_items(host, &state.root, &t.calendars_field)? {
            let key = try_read_text(host, &at, &t.calendar, "key")?;
            let account = try_read_text(host, &at, &t.calendar, "account")?;
            if account.is_empty() {
                // A calendar of the workspace's own: no server is ever asked about it.
                continue;
            }
            let record = at.id_string();
            state
                .by_key
                .insert((account.clone(), key.clone()), record.clone());
            state.calendars.insert(
                record,
                Cal {
                    key,
                    account,
                    token: try_read_text(host, &at, &t.calendar, "sync_token")?,
                    name: try_read_text(host, &at, &t.calendar, "name")?,
                    color: try_read_text(host, &at, &t.calendar, "color")?,
                },
            );
        }
        for at in try_items(host, &state.root, &t.events_field)? {
            let record = at.id_string();
            let href = try_read_text(host, &at, &t.event, "href")?;
            let master = try_read_ref(host, &at, &t.event, "recurrence_of")?
                .map(|master| master.id_string());
            let original_start = try_read_text(host, &at, &t.event, "original_start")?;
            let Some(calendar) = try_read_ref(host, &at, &t.event, "calendar")? else {
                continue;
            };
            let Some(cal) = state.calendars.get(&calendar.id_string()).cloned() else {
                continue;
            };
            if href.is_empty() && master.is_none() {
                // An entry the origin has not written to a server: the workspace's own.
                continue;
            }
            if let Some(master) = &master {
                state
                    .by_href
                    .entry((cal.account.clone(), href.clone()))
                    .or_insert_with(|| master.clone());
            } else {
                state
                    .by_href
                    .insert((cal.account.clone(), href.clone()), record.clone());
            }
            let uid = try_read_text(host, &at, &t.event, "key")?;
            let confirmed = read_entry(host, &state, &at, &t.event).unwrap_or_default();
            if master.is_none() {
                state
                    .by_uid
                    .insert((cal.account.clone(), uid), record.clone());
            }
            if let Some(field) = t.event.field("attendees") {
                let mut people = HashMap::new();
                for guest in try_items(host, &at, field)? {
                    let email = try_read_text(host, &guest, &t.attendee, "email")?;
                    people.insert(email, guest.id_string());
                }
                state.attendees.insert(record.clone(), people);
            }
            state.events.insert(
                record,
                Ev {
                    account: cal.account.clone(),
                    calendar: cal.key.clone(),
                    href,
                    confirmed,
                    master,
                    original_start,
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
            let state = match Self::load(cx.host, types, root.clone()) {
                Ok(state) => state,
                Err(error) => {
                    tracing::warn!(%error, "the calendars could not be read");
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
        polled.wrote |= self.follow_accounts(cx.host, &cx.now);
        polled.wrote |= self.push(cx.host, &cx.now);
        polled.wrote |= self.take_again(cx.host, &cx.now);
        let events = self.take_events(cx.host, &cx.now);
        polled.wrote |= events.wrote;
        polled.settle |= events.settle;
        polled.wrote |= self.moved(cx.host, &cx.now);
        if self.reload {
            return polled;
        }
        polled.settle |= self.reconnect(cx.host);
        for account in std::mem::take(&mut self.due) {
            let known = self.known_of(&account);
            self.checking.insert(account.clone());
            if self.asked.insert(account.clone()) && self.connector.sync(&account, known).is_none()
            {
                self.checking.remove(&account);
                tracing::warn!(
                    account,
                    "the calendar connector took no request for the account"
                );
            }
        }
        polled.wrote |= self.write_account_state(cx.host, &cx.now);
        polled
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one capability after another, each asking its server or saying why it cannot"
    )]
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
            .copied()
            .unwrap_or(true)
        {
            return Delivery::Rejected(READ_ONLY.to_owned());
        }
        // The person whose answer it is, as their own record.
        let mut answer = None;
        let asked = match capability {
            "caldav.answer" => {
                let uid = field(1);
                let attendee = field(2);
                let given = field(3);
                let Some((record, event)) = self.state.as_ref().and_then(|state| {
                    let record = state.record_of(&account, &uid)?;
                    let event = state.events.get(&record).cloned();
                    Some((record, event))
                }) else {
                    return Delivery::Rejected(
                        "the calendar holds no event of that account under that name".to_owned(),
                    );
                };
                let Some(event) = event else {
                    return Delivery::Rejected(
                        "the calendar holds no event of that account under that name".to_owned(),
                    );
                };
                answer = Some((record, attendee.clone(), given.clone()));
                self.connector
                    .answer(&account, &event.calendar, &event.href, &attendee, &given)
            }
            "caldav.import" => {
                let calendar = field(1);
                let owned = self
                    .state
                    .as_ref()
                    .and_then(|state| state.calendar_ptr(&calendar).map(str::to_owned));
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
                self.pending.insert(
                    request,
                    Pending::Asked {
                        key: key.clone(),
                        account,
                        answer,
                    },
                );
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
