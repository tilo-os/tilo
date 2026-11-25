//! The connector over `CalDAV` in native code, where the network and the credentials are: the
//! server's listings read into entries, and a person's change to an entry written back with the
//! version it goes over. It runs on a worker thread the connector process starts
//! ([`crate::serve`]) and speaks [`tilo_dav`]'s blocking client, so no request holds the
//! connector's loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use crate::ics;
use crate::model::{Attendee, Calendar, Event, Id, InviteStatus, OccurrenceOverride, Recurrence};
use jiff::civil::{Date, DateTime, Time};
use tilo_dav::{
    Credentials, DavClient, Depth, Limits, Options, Precondition, Redirects, Url, props,
};

use crate::connector::{
    AccountRef, CalDavConnector, CalendarKnown, CalendarRef, Entry, Event as Reported, Guest,
    Occurrence, Removal, Request, Sync, Unmade,
};

/// The limits a listing is read under: the same safety caps every `CalDAV` reader here holds.
const LIMITS: Limits = Limits {
    max_bytes: 8 * 1024 * 1024,
    max_responses: 10_000,
    max_depth: 32,
    max_nodes: 1_000_000,
    max_href_bytes: 4096,
};

/// The largest one entry may be.
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;

/// What a listing asks of each entry: its version and its text.
const REPORTED: [tilo_dav::PropName; 2] = [props::GETETAG, props::CALENDAR_DATA];

/// What an entry is sent as.
const ENTRY_TYPE: &str = "text/calendar; charset=utf-8";

/// What a property write is sent as.
const XML_TYPE: &str = "application/xml; charset=utf-8";

/// Apple's calendar property namespace: the one a calendar's colour is kept in.
const APPLE_CALDAV: &str = "http://apple.com/ns/ical/";

/// The colour a calendar says it is, as its server keeps it.
fn calendar_color() -> tilo_dav::PropName {
    tilo_dav::PropName {
        namespace: APPLE_CALDAV,
        name: "calendar-color",
    }
}

/// The `PROPPATCH` body that sets a calendar's name and colour.
fn property_update(name: &str, color: &str) -> String {
    let escape = quick_xml_escape;
    let mut props = String::new();
    if !name.is_empty() {
        props.push_str("<D:displayname>");
        props.push_str(&escape(name));
        props.push_str("</D:displayname>");
    }
    if !color.is_empty() {
        props.push_str("<A:calendar-color>");
        props.push_str(&escape(color));
        props.push_str("</A:calendar-color>");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:propertyupdate xmlns:D=\"DAV:\" \
         xmlns:A=\"{APPLE_CALDAV}\"><D:set><D:prop>{props}</D:prop></D:set></D:propertyupdate>"
    )
}

/// `text` with the characters an XML text may not hold written as entities.
fn quick_xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// What a person is told where writing an entry would have its calendar server call the event
/// off for the people it names (RFC 6638, 3.2.3.1).
const WOULD_CALL_IT_OFF: &str = "the calendar server writes to the people this entry names, and \
     the entry here no longer says who organises it: writing it would have the server call it \
     off, so nothing was written to it";

/// What every writing to the server of an account connected for reading only is refused with.
const READ_ONLY: &str = "This calendar account is connected for reading only, so nothing is \
                         written to its calendar server.";

/// An account the connector signs in to: its settings, with the password the session released
/// for it, or the session's words for why it released none.
#[derive(Clone)]
pub struct CalendarAccount {
    pub id: String,
    pub name: String,
    pub server: String,
    pub username: String,
    pub password: String,
    pub refused: Option<String>,
    pub read_only: bool,
}

/// Where the accounts come from: the session's account sign-ins ([`crate::sign_ins`]), or a
/// test's.
pub type AccountSource = Arc<dyn Fn() -> Vec<CalendarAccount> + Send + std::marker::Sync>;

/// What wakes the connector's loop when the engine has an answer.
pub type Wake = Arc<dyn Fn() + Send + std::marker::Sync>;

/// One request the engine works on.
pub struct EngineRequest {
    pub id: Request,
    pub account: String,
    pub work: Work,
}

/// What a request asks of a server.
pub enum Work {
    Sync {
        known: Vec<CalendarKnown>,
    },
    Put {
        entry: Box<Entry>,
        href: String,
        from: String,
    },
    Remove {
        calendar: String,
        href: String,
    },
    Calendar {
        calendar: String,
        name: String,
        color: String,
    },
    Answer {
        calendar: String,
        href: String,
        attendee: String,
        answer: String,
    },
    Import {
        calendar: String,
        text: String,
    },
}

/// What the engine answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Synced(Sync),
    Accepted { request: Request, href: String },
    Refused { request: Request, reason: String },
    Unsent { request: Request, reason: String },
    Unreachable { account: String, reason: String },
}

/// The connector over the calendar engine, which works requests one at a time on a worker
/// thread ([`spawn`]) and holds what each account's server answered so far.
pub struct EngineConnector {
    source: AccountSource,
    requests: Sender<EngineRequest>,
    /// The engine's answers, when this connector is the one reading them; else whoever reads
    /// them hands this connector its own ([`Self::engine_event`]).
    events: Option<Receiver<EngineEvent>>,
    /// Every request this connector made and has not been answered about.
    pending: std::collections::BTreeSet<Request>,
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
            pending: std::collections::BTreeSet::new(),
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
            pending: std::collections::BTreeSet::new(),
        }
    }

    /// Whether the engine still works on a request of this connector's.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether `request` is one this connector made and is waiting on.
    #[must_use]
    pub fn owns(&self, request: &Request) -> bool {
        self.pending.contains(request)
    }

    fn request(&mut self, account: &str, work: Work) -> Result<Request, Unmade> {
        if !(self.source)().iter().any(|known| known.id == account) {
            return Err(Unmade::NoAccount);
        }
        let id = uuid::Uuid::new_v4();
        if self
            .requests
            .send(EngineRequest {
                id,
                account: account.to_owned(),
                work,
            })
            .is_err()
        {
            tracing::error!(
                account,
                "the calendar engine stopped: a request was not made"
            );
            return Err(Unmade::EngineStopped);
        }
        self.pending.insert(id);
        Ok(id)
    }

    /// Take in what the engine said, adding what it means to `out`.
    pub fn engine_event(&mut self, event: EngineEvent, out: &mut Vec<Reported>) {
        match &event {
            EngineEvent::Accepted { request, .. }
            | EngineEvent::Refused { request, .. }
            | EngineEvent::Unsent { request, .. } => {
                self.pending.remove(request);
            }
            EngineEvent::Synced(_) | EngineEvent::Unreachable { .. } => {}
        }
        out.push(match event {
            EngineEvent::Synced(sync) => Reported::Synced(sync),
            EngineEvent::Accepted { request, href } => Reported::Accepted { request, href },
            EngineEvent::Refused { request, reason } => Reported::Refused { request, reason },
            EngineEvent::Unsent { request, reason } => Reported::Unsent { request, reason },
            EngineEvent::Unreachable { account, reason } => {
                Reported::Unreachable { account, reason }
            }
        });
    }
}

/// A connector that works the engine on the caller's own thread, answering every request before
/// it returns: what a test drives, and a reader that would rather wait in its own call.
pub struct DirectConnector {
    engine: Engine,
    answered: Vec<Reported>,
}

impl DirectConnector {
    /// The connector reading its accounts from `source`.
    #[must_use]
    pub fn new(source: AccountSource) -> Self {
        Self {
            engine: Engine::new(source),
            answered: Vec::new(),
        }
    }

    fn asked(&mut self, account: &str, work: Work) -> Result<Request, Unmade> {
        if !(self.engine.source)()
            .iter()
            .any(|known| known.id == account)
        {
            return Err(Unmade::NoAccount);
        }
        let request = uuid::Uuid::new_v4();
        for event in self.engine.perform(EngineRequest {
            id: request,
            account: account.to_owned(),
            work,
        }) {
            let mut answered = Vec::new();
            let reported = match event {
                EngineEvent::Synced(sync) => Reported::Synced(sync),
                EngineEvent::Accepted { request, href } => Reported::Accepted { request, href },
                EngineEvent::Refused { request, reason } => Reported::Refused { request, reason },
                EngineEvent::Unsent { request, reason } => Reported::Unsent { request, reason },
                EngineEvent::Unreachable { account, reason } => {
                    Reported::Unreachable { account, reason }
                }
            };
            answered.push(reported);
            self.answered.append(&mut answered);
        }
        Ok(request)
    }
}

impl CalDavConnector for DirectConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        (self.engine.source)()
            .into_iter()
            .map(|signed| AccountRef {
                id: signed.id,
                address: signed.username,
                name: if signed.name.is_empty() {
                    signed.server.clone()
                } else {
                    signed.name
                },
                server: signed.server,
                read_only: signed.read_only,
            })
            .collect()
    }

    fn sync(&mut self, account: &str, known: Vec<CalendarKnown>) -> Option<Request> {
        self.asked(account, Work::Sync { known }).ok()
    }

    fn put(
        &mut self,
        account: &str,
        entry: Entry,
        href: &str,
        from: &str,
    ) -> Result<Request, Unmade> {
        self.asked(
            account,
            Work::Put {
                entry: Box::new(entry),
                href: href.to_owned(),
                from: from.to_owned(),
            },
        )
    }

    fn remove(&mut self, account: &str, calendar: &str, href: &str) -> Result<Request, Unmade> {
        self.asked(
            account,
            Work::Remove {
                calendar: calendar.to_owned(),
                href: href.to_owned(),
            },
        )
    }

    fn set_calendar(
        &mut self,
        account: &str,
        calendar: &str,
        name: &str,
        color: &str,
    ) -> Result<Request, Unmade> {
        self.asked(
            account,
            Work::Calendar {
                calendar: calendar.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
            },
        )
    }

    fn answer(
        &mut self,
        account: &str,
        calendar: &str,
        href: &str,
        attendee: &str,
        answer: &str,
    ) -> Result<Request, Unmade> {
        self.asked(
            account,
            Work::Answer {
                calendar: calendar.to_owned(),
                href: href.to_owned(),
                attendee: attendee.to_owned(),
                answer: answer.to_owned(),
            },
        )
    }

    fn import(&mut self, account: &str, calendar: &str, text: &str) -> Result<Request, Unmade> {
        self.asked(
            account,
            Work::Import {
                calendar: calendar.to_owned(),
                text: text.to_owned(),
            },
        )
    }

    fn events(&mut self) -> Vec<Reported> {
        std::mem::take(&mut self.answered)
    }
}

impl CalDavConnector for EngineConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        (self.source)()
            .into_iter()
            .map(|signed| AccountRef {
                id: signed.id,
                address: signed.username,
                name: if signed.name.is_empty() {
                    signed.server.clone()
                } else {
                    signed.name
                },
                server: signed.server,
                read_only: signed.read_only,
            })
            .collect()
    }

    fn sync(&mut self, account: &str, known: Vec<CalendarKnown>) -> Option<Request> {
        self.request(account, Work::Sync { known }).ok()
    }

    fn put(
        &mut self,
        account: &str,
        entry: Entry,
        href: &str,
        from: &str,
    ) -> Result<Request, Unmade> {
        self.request(
            account,
            Work::Put {
                entry: Box::new(entry),
                href: href.to_owned(),
                from: from.to_owned(),
            },
        )
    }

    fn remove(&mut self, account: &str, calendar: &str, href: &str) -> Result<Request, Unmade> {
        self.request(
            account,
            Work::Remove {
                calendar: calendar.to_owned(),
                href: href.to_owned(),
            },
        )
    }

    fn set_calendar(
        &mut self,
        account: &str,
        calendar: &str,
        name: &str,
        color: &str,
    ) -> Result<Request, Unmade> {
        self.request(
            account,
            Work::Calendar {
                calendar: calendar.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
            },
        )
    }

    fn answer(
        &mut self,
        account: &str,
        calendar: &str,
        href: &str,
        attendee: &str,
        answer: &str,
    ) -> Result<Request, Unmade> {
        self.request(
            account,
            Work::Answer {
                calendar: calendar.to_owned(),
                href: href.to_owned(),
                attendee: attendee.to_owned(),
                answer: answer.to_owned(),
            },
        )
    }

    fn import(&mut self, account: &str, calendar: &str, text: &str) -> Result<Request, Unmade> {
        self.request(
            account,
            Work::Import {
                calendar: calendar.to_owned(),
                text: text.to_owned(),
            },
        )
    }

    fn events(&mut self) -> Vec<Reported> {
        let mut out = Vec::new();
        while let Some(event) = self.next_event(&mut out) {
            self.engine_event(event, &mut out);
        }
        out
    }
}

/// Why what the engine held is answered unmade, once its worker ended.
const WHY: &str = "the calendar engine stopped";

impl EngineConnector {
    fn next_event(&self, out: &mut Vec<Reported>) -> Option<EngineEvent> {
        match self.events.as_ref()?.try_recv() {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Self::engine_stopped(out);
                None
            }
        }
    }

    fn engine_stopped(out: &mut Vec<Reported>) {
        out.push(Reported::Unreachable {
            account: String::new(),
            reason: WHY.to_owned(),
        });
    }
}

/// Start the engine on a worker thread of its own, reading its accounts from `source` and
/// calling `wake` when it has an answer.
pub fn spawn(source: AccountSource, wake: Wake) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    let (requests, asked) = std::sync::mpsc::channel();
    let (answered, events) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("calendar-engine".into())
        .spawn(move || {
            let mut engine = Engine::new(source);
            while let Ok(request) = asked.recv() {
                for event in engine.perform(request) {
                    if answered.send(event).is_err() {
                        return;
                    }
                }
                wake();
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "the calendar engine does not start");
    }
    (requests, events)
}

/// What an account's server said so far: its calendars, where each pull ended, and each entry
/// as it was read.
#[derive(Default)]
struct Held {
    calendars: Vec<CalendarRef>,
    /// The token each calendar's last pull ended at, by its key.
    tokens: HashMap<String, Option<String>>,
    /// Each entry's version and what it said, by its calendar's key and its href.
    entries: HashMap<(String, String), HeldEntry>,
}

struct HeldEntry {
    etag: String,
    entry: Entry,
}

/// The engine: what each account's server holds, and the requests it works on.
pub struct Engine {
    source: AccountSource,
    held: HashMap<String, Held>,
}

impl Engine {
    fn new(source: AccountSource) -> Self {
        Self {
            source,
            held: HashMap::new(),
        }
    }

    fn account(&self, id: &str) -> Option<CalendarAccount> {
        (self.source)().into_iter().find(|account| account.id == id)
    }

    fn perform(&mut self, request: EngineRequest) -> Vec<EngineEvent> {
        let EngineRequest { id, account, work } = request;
        let Some(signed) = self.account(&account) else {
            return vec![EngineEvent::Refused {
                request: id,
                reason: format!("no calendar account {account} in the workspace's accounts"),
            }];
        };
        match work {
            Work::Sync { known } => match self.sync(&signed, known) {
                Ok(sync) => vec![EngineEvent::Synced(sync)],
                Err(reason) => vec![EngineEvent::Unreachable { account, reason }],
            },
            Work::Put { entry, href, from } => {
                let written = self.put(&signed, &entry, &href, &from);
                Self::answer_write(id, written)
            }
            Work::Remove { calendar, href } => {
                let written = self.remove(&signed, &calendar, &href);
                Self::answer_write(id, written)
            }
            Work::Calendar {
                calendar,
                name,
                color,
            } => {
                let written = self.set_calendar(&signed, &calendar, &name, &color);
                Self::answer_write(id, written)
            }
            Work::Answer {
                calendar,
                href,
                attendee,
                answer,
            } => {
                let written = self.answer_invitation(&signed, &calendar, &href, &attendee, &answer);
                Self::answer_write(id, written)
            }
            Work::Import { calendar, text } => {
                let written = self.import(&signed, &calendar, &text);
                Self::answer_write(id, written)
            }
        }
    }

    /// What a write answered, as the connector's event: the server's words for a refusal, and
    /// they say whether the server was reached at all.
    fn answer_write(request: Request, written: Result<Written, String>) -> Vec<EngineEvent> {
        match written {
            Ok(Written::Stored(href)) => vec![EngineEvent::Accepted { request, href }],
            Ok(Written::Removed) => vec![EngineEvent::Accepted {
                request,
                href: String::new(),
            }],
            Err(reason) if reason == UNREACHED => vec![EngineEvent::Unsent { request, reason }],
            Err(reason) => vec![EngineEvent::Refused { request, reason }],
        }
    }

    /// What the account's server holds: its calendars, and what changed in each since the last
    /// pull.
    #[allow(
        clippy::too_many_lines,
        reason = "one calendar server's whole answer, each calendar in turn"
    )]
    fn sync(
        &mut self,
        signed: &CalendarAccount,
        known: Vec<CalendarKnown>,
    ) -> Result<Sync, String> {
        let (client, server) = Self::client(signed)?;
        let discovered = tilo_dav::discovery::calendars(&client, &server)?;
        let calendars: Vec<CalendarRef> = discovered
            .into_iter()
            .map(|found| CalendarRef {
                account: signed.id.clone(),
                key: found.href,
                name: shown_name(&found.name),
                color: String::new(),
                writable: !found.read_only && !signed.read_only,
                token: String::new(),
            })
            .collect();
        let mut held = self.held.remove(&signed.id).unwrap_or_default();
        if held.calendars.is_empty() {
            // A connector restarted since the workspace last pulled: it asks from where that
            // pull ended, and writes over the versions the workspace kept.
            for kept in known {
                if !kept.calendar.is_empty() {
                    held.tokens.insert(
                        kept.calendar.clone(),
                        (!kept.token.is_empty()).then_some(kept.token),
                    );
                }
                for (href, etag) in kept.entries {
                    held.entries.insert(
                        (kept.calendar.clone(), href),
                        HeldEntry {
                            etag,
                            entry: Entry::default(),
                        },
                    );
                }
            }
        }
        // What a calendar no longer holds is forgotten here: its entries are gone from the
        // server, and the origin takes them out of the workspace with this pull.
        held.calendars.clone_from(&calendars);
        let keys: Vec<String> = calendars.iter().map(|c| c.key.clone()).collect();
        held.tokens.retain(|key, _| keys.contains(key));
        held.entries.retain(|(key, _), _| keys.contains(key));
        let mut sync = Sync {
            account: signed.id.clone(),
            calendars,
            ..Sync::default()
        };
        for index in 0..sync.calendars.len() {
            let Some(calendar) = sync.calendars.get(index).cloned() else {
                continue;
            };
            let collection = Url::parse(&calendar.key)
                .map_err(|_| "a calendar's address is not an address".to_owned())?;
            if collection.origin() != server.origin() {
                continue;
            }
            if let Some(color) = Self::calendar_color_of(&client, &collection)
                && let Some(calendar) = sync.calendars.get_mut(index)
            {
                calendar.color = color;
            }
            let token = held.tokens.get(&calendar.key).cloned().flatten();
            let (listing, whole, tells) = Self::list(&client, &collection, token.as_deref())?;
            let mut seen: Vec<String> = Vec::new();
            for response in &listing.responses {
                let Ok(url) = collection.join(&response.href) else {
                    continue;
                };
                if url.origin() != server.origin() {
                    continue;
                }
                let href = url.to_string();
                if response.status == Some(404) {
                    if held
                        .entries
                        .remove(&(calendar.key.clone(), href.clone()))
                        .is_some()
                    {
                        sync.removed.push(Removal {
                            calendar: calendar.key.clone(),
                            href,
                        });
                    }
                    continue;
                }
                let Some(text) = response
                    .prop("calendar-data")
                    .and_then(|data| data.text.as_deref())
                else {
                    continue;
                };
                if text.len() > MAX_ENTRY_BYTES {
                    return Err("an entry of the calendar is larger than 2 MiB".to_owned());
                }
                let etag = response.prop_text("getetag").unwrap_or_default().to_owned();
                seen.push(href.clone());
                let Some(mut entry) = Self::entry_of(text, &calendar.key, &href) else {
                    continue;
                };
                etag.clone_into(&mut entry.etag);
                sync.entries.push(entry.clone());
                held.entries
                    .insert((calendar.key.clone(), href), HeldEntry { etag, entry });
            }
            if whole {
                let gone: Vec<(String, String)> = held
                    .entries
                    .keys()
                    .filter(|(key, href)| key == &calendar.key && !seen.contains(href))
                    .cloned()
                    .collect();
                for (key, href) in gone {
                    held.entries.remove(&(key.clone(), href.clone()));
                    sync.removed.push(Removal {
                        calendar: key,
                        href,
                    });
                }
            }
            let token = match listing.sync_token.clone().filter(|_| tells) {
                Some(token) => Some(token),
                None if tells => None,
                None => Self::token_of(&client, &collection)?,
            };
            if let Some(kept) = token.as_deref()
                && let Some(calendar) = sync.calendars.get_mut(index)
            {
                kept.clone_into(&mut calendar.token);
            }
            held.tokens.insert(calendar.key.clone(), token);
        }
        self.held.insert(signed.id.clone(), held);
        Ok(sync)
    }

    /// One calendar's listing: the sync report from `token`, or everything it holds where its
    /// server does not tell what changed.
    fn list(
        client: &DavClient,
        collection: &Url,
        token: Option<&str>,
    ) -> Result<(tilo_dav::Multistatus, bool, bool), String> {
        let said =
            |error: tilo_dav::client::Error| format!("the calendar server did not answer: {error}");
        let asked = tilo_dav::body::sync_collection_body(token.unwrap_or_default(), &REPORTED);
        let mut response = client
            .report(collection.clone(), Depth::One, asked)
            .map_err(|error| {
                if is_unreached(&error) {
                    UNREACHED.to_owned()
                } else {
                    said(error)
                }
            })?;
        let mut whole = token.is_none();
        let mut tells = true;
        if matches!(response.status().as_u16(), 400 | 403 | 405 | 409 | 501) {
            whole = true;
            tells = false;
            let everything = tilo_dav::body::calendar_query_body(&REPORTED, None);
            response = client
                .report(collection.clone(), Depth::One, everything)
                .map_err(|error| {
                    if is_unreached(&error) {
                        UNREACHED.to_owned()
                    } else {
                        said(error)
                    }
                })?;
        }
        if !response.status().is_success() {
            return Err(format!(
                "the calendar server answered {}",
                response.status().as_u16()
            ));
        }
        let listing = response
            .multistatus(&LIMITS)
            .map_err(|error| format!("the calendar server's answer does not read: {error}"))?;
        Ok((listing, whole, tells))
    }

    /// The colour a calendar says it is, where its server keeps one.
    fn calendar_color_of(client: &DavClient, collection: &Url) -> Option<String> {
        let response = client
            .propfind(collection.clone(), Depth::Zero, &[calendar_color()])
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let listing = response.multistatus(&LIMITS).ok()?;
        listing
            .responses
            .iter()
            .find_map(|response| response.prop_text("calendar-color"))
            .map(ToOwned::to_owned)
    }

    /// The token a calendar's collection is at now, where its server gives one.
    fn token_of(client: &DavClient, collection: &Url) -> Result<Option<String>, String> {
        let response = client
            .propfind(collection.clone(), Depth::Zero, &[props::SYNC_TOKEN])
            .map_err(|error| format!("the calendar server did not answer: {error}"))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let listing = response
            .multistatus(&LIMITS)
            .map_err(|error| format!("the calendar server's answer does not read: {error}"))?;
        Ok(listing
            .responses
            .iter()
            .find_map(|response| response.prop_text("sync-token"))
            .map(ToOwned::to_owned))
    }

    /// What the entry `text` says, as the record holds it.
    pub fn entry_of(text: &str, calendar: &str, href: &str) -> Option<Entry> {
        let parsed = ics::parse_calendar(text);
        let mut event = parsed.events.into_iter().next()?;
        event.calendar_id = Some(calendar.to_owned());
        Some(entry_of(
            &event,
            &ics::notes(text),
            uid_of(text),
            calendar,
            href,
            text,
        ))
    }

    /// Write a calendar's own properties: its `name` and its `color`, as its server keeps them.
    fn set_calendar(
        &mut self,
        signed: &CalendarAccount,
        calendar: &str,
        name: &str,
        color: &str,
    ) -> Result<Written, String> {
        if signed.read_only {
            return Err(READ_ONLY.to_owned());
        }
        let (client, server) = Self::client(signed)?;
        let url = Url::parse(calendar)
            .map_err(|_| "a calendar's address is not an address".to_owned())?;
        if url.origin() != server.origin() {
            return Err("a calendar is on its account's server".to_owned());
        }
        let response = client
            .send(
                "PROPPATCH",
                url.as_str(),
                &[("Content-Type", XML_TYPE)],
                tilo_dav::Body::Bytes(property_update(name, color).into_bytes()),
                Precondition::None,
            )
            .map_err(|error| {
                if is_unreached(&error) {
                    UNREACHED.to_owned()
                } else {
                    format!("the calendar server did not answer: {error}")
                }
            })?;
        if response.status().as_u16() == 403 {
            return Err("That calendar is read-only".to_owned());
        }
        if !response.status().is_success() {
            return Err(format!(
                "the calendar server answered {}",
                response.status().as_u16()
            ));
        }
        // A `207` says each property was written or not, one status a property.
        if let Ok(listing) = response.multistatus(&LIMITS)
            && let Some(status) = listing
                .responses
                .iter()
                .flat_map(|response| response.propstats.iter())
                .find_map(|propstat| propstat.status.filter(|status| *status >= 300))
        {
            return Err(format!("the calendar server answered {status}"));
        }
        if let Some(held) = self.held.get_mut(&signed.id)
            && let Some(calendar) = held.calendars.iter_mut().find(|held| held.key == calendar)
        {
            name.clone_into(&mut calendar.name);
            color.clone_into(&mut calendar.color);
        }
        Ok(Written::Stored(calendar.to_owned()))
    }

    /// Write `entry` to its server: over the entry at `href`, or as a new one.
    fn put(
        &mut self,
        signed: &CalendarAccount,
        entry: &Entry,
        href: &str,
        from: &str,
    ) -> Result<Written, String> {
        if signed.read_only {
            return Err(READ_ONLY.to_owned());
        }
        let (client, server) = Self::client(signed)?;
        if !from.is_empty() {
            // The entry moves: it goes from where its server keeps it, and its server answers
            // nobody for its person.
            delete(&client, from)?;
            self.forget(&signed.id, from);
        }
        let collection = Url::parse(&entry.calendar)
            .map_err(|_| "a calendar's address is not an address".to_owned())?;
        if collection.origin() != server.origin() {
            return Err("a calendar is on its account's server".to_owned());
        }
        let mut body = text_of(entry);
        let mut version: Option<String> = None;
        let mut creates = false;
        let url = if href.is_empty() {
            creates = true;
            let url = format!(
                "{}/{}.ics",
                collection.as_str().trim_end_matches('/'),
                entry.uid
            );
            if names_somebody(&body) {
                body = ics::kept_text(&body);
            }
            url
        } else {
            let url =
                Url::parse(href).map_err(|_| "an entry's address is not an address".to_owned())?;
            if url.origin() != server.origin() {
                return Err("an entry is on its account's server".to_owned());
            }
            let held = match held_now(&client, &url, self.version(&signed.id, href).as_deref()) {
                Ok((text, version)) => (text, version),
                Err(Unread::Gone) => {
                    return Err(
                        "This event is no longer on its server: it is taken out of the workspace \
                         with the next sync"
                            .to_owned(),
                    );
                }
                Err(Unread::Changed) => {
                    return Err(
                        "This event changed on its server meanwhile: the change there is taken in \
                         with the next sync"
                            .to_owned(),
                    );
                }
                Err(Unread::Failed(reason)) => return Err(reason),
            };
            body = ics::exported_beside(&body, &held.0);
            if ics::ends_what_its_server_schedules(&body, &held.0) {
                return Err(WOULD_CALL_IT_OFF.to_owned());
            }
            version = Some(held.1);
            url.to_string()
        };
        let precondition = match version.as_deref() {
            Some(version) => Precondition::IfMatch(version),
            None if creates => Precondition::IfNoneMatchAny,
            None => Precondition::None,
        };
        let response = client
            .put(
                url.as_str(),
                body.into_bytes(),
                Some(ENTRY_TYPE),
                precondition,
            )
            .map_err(|error| {
                if is_unreached(&error) {
                    UNREACHED.to_owned()
                } else {
                    format!("the calendar server did not answer: {error}")
                }
            })?;
        match response.status().as_u16() {
            412 => Err(
                "This event changed on its server meanwhile: the change there is taken in with \
                 the next sync"
                    .to_owned(),
            ),
            403 => Err("That calendar is read-only".to_owned()),
            _ if response.status().is_success() => {
                let etag = response.etag().unwrap_or_default();
                self.remember(signed, entry, &url, &etag);
                Ok(Written::Stored(url))
            }
            status => Err(format!("the calendar server answered {status}")),
        }
    }

    /// Take the entry at `href` out of its server.
    fn remove(
        &mut self,
        signed: &CalendarAccount,
        calendar: &str,
        href: &str,
    ) -> Result<Written, String> {
        let _ = calendar;
        if signed.read_only {
            return Err(READ_ONLY.to_owned());
        }
        let (client, server) = Self::client(signed)?;
        let url =
            Url::parse(href).map_err(|_| "an entry's address is not an address".to_owned())?;
        if url.origin() != server.origin() {
            return Err("an entry is on its account's server".to_owned());
        }
        delete(&client, href)?;
        self.forget(&signed.id, href);
        Ok(Written::Removed)
    }

    /// Answer the invitation at `href` as `attendee`.
    fn answer_invitation(
        &mut self,
        signed: &CalendarAccount,
        calendar: &str,
        href: &str,
        attendee: &str,
        answer: &str,
    ) -> Result<Written, String> {
        if signed.read_only {
            return Err(READ_ONLY.to_owned());
        }
        let (client, server) = Self::client(signed)?;
        let url =
            Url::parse(href).map_err(|_| "an entry's address is not an address".to_owned())?;
        if url.origin() != server.origin() {
            return Err("an entry is on its account's server".to_owned());
        }
        let (held, version) =
            match held_now(&client, &url, self.version(&signed.id, href).as_deref()) {
                Ok(held) => held,
                Err(Unread::Gone) => {
                    return Err("This invitation is no longer on its server".to_owned());
                }
                Err(Unread::Changed) => {
                    return Err("This invitation changed on its server meanwhile".to_owned());
                }
                Err(Unread::Failed(reason)) => return Err(reason),
            };
        let Some(body) = with_answer(&held, attendee, answer) else {
            return Err(format!("This invitation names nobody at {attendee}"));
        };
        let response = client
            .put(
                url.as_str(),
                body.clone().into_bytes(),
                Some(ENTRY_TYPE),
                Precondition::IfMatch(&version),
            )
            .map_err(|error| {
                if is_unreached(&error) {
                    UNREACHED.to_owned()
                } else {
                    format!("the calendar server did not answer: {error}")
                }
            })?;
        match response.status().as_u16() {
            412 => Err("This invitation changed on its server meanwhile".to_owned()),
            _ if response.status().is_success() => {
                if let Some(entry) = Self::entry_of(&body, calendar, href) {
                    let etag = response.etag().unwrap_or_else(|| version.clone());
                    if let Some(held) = self.held.get_mut(&signed.id) {
                        held.entries.insert(
                            (calendar.to_owned(), href.to_owned()),
                            HeldEntry { etag, entry },
                        );
                    }
                }
                Ok(Written::Stored(href.to_owned()))
            }
            status => Err(format!("the calendar server answered {status}")),
        }
    }

    /// Take the invitation `text` into `calendar`: one entry under the invitation's own `UID`,
    /// however often it comes. The first copy puts it in, the same again changes nothing, a later
    /// version of it brings the entry up to date, and its cancellation takes the entry out. What
    /// goes up tells the server to write to nobody, and the removal tells it to answer nobody
    /// ([`ics::kept_beside`], [`delete`]): the person's calendar takes the invitation in, and
    /// nobody is written to in their name for that.
    fn import(
        &mut self,
        signed: &CalendarAccount,
        calendar: &str,
        text: &str,
    ) -> Result<Written, String> {
        if signed.read_only {
            return Err(READ_ONLY.to_owned());
        }
        let ics::Invitation { cancels, .. } = ics::invitation(text)?;
        let uid = uid_of(text).ok_or_else(|| "The invitation names no event".to_owned())?;
        let (client, server) = Self::client(signed)?;
        let collection = Url::parse(calendar)
            .map_err(|_| "a calendar's address is not an address".to_owned())?;
        if collection.origin() != server.origin() {
            return Err("a calendar is on its account's server".to_owned());
        }
        let held = self.held.get(&signed.id).and_then(|held| {
            held.entries
                .iter()
                .find(|((key, _), held)| key == calendar && held.entry.uid == uid)
                .map(|(_, held)| held.entry.href.clone())
        });
        let Some(href) = held else {
            if cancels {
                // Nothing of it is here to take out.
                return Ok(Written::Removed);
            }
            let body = ics::kept_text(text);
            let url = format!("{}/{}.ics", collection.as_str().trim_end_matches('/'), uid);
            return self.store_invitation(
                signed,
                (&client, calendar, &url),
                body,
                Precondition::IfNoneMatchAny,
            );
        };
        let url =
            Url::parse(&href).map_err(|_| "an entry's address is not an address".to_owned())?;
        if url.origin() != server.origin() {
            return Err("an entry is on its account's server".to_owned());
        }
        if cancels {
            // Still held here: the sync that follows the import finds it gone, and the workspace
            // takes it out with that.
            delete(&client, &href)?;
            return Ok(Written::Removed);
        }
        let (held, version) =
            match held_now(&client, &url, self.version(&signed.id, &href).as_deref()) {
                Ok(held) => held,
                Err(Unread::Gone) => {
                    return Err(
                        "This invitation is no longer on its server: it is taken out of the \
                         workspace with the next sync"
                            .to_owned(),
                    );
                }
                Err(Unread::Changed) => {
                    return Err(
                        "This invitation changed on its server meanwhile: the change there is \
                         taken in with the next sync"
                            .to_owned(),
                    );
                }
                Err(Unread::Failed(reason)) => return Err(reason),
            };
        // Each line naming somebody says of who writes to them what the server holds of it now
        // (RFC 6638, 3.2.1.2), so the version that goes up starts and calls off nothing.
        let mut body = ics::kept_beside(text, Some(&held));
        let sent = ics::parse(&body).0.into_iter().next();
        let kept = ics::parse(&held).0.into_iter().next();
        if let (Some(mut sent), Some(kept)) = (sent, kept.clone()) {
            sent.calendar_id.clone_from(&kept.calendar_id);
            if sent == kept {
                // The same invitation again: the person's answer and what the server wrote stay.
                return Ok(Written::Stored(href));
            }
        }
        // What its person filed it under on the server stays, where the sender files it under
        // nothing.
        let filed: Vec<String> = kept
            .map(|kept| kept.categories.iter().cloned().collect())
            .unwrap_or_default();
        let sends_none = ics::parse(&body)
            .0
            .first()
            .is_none_or(|sent| sent.categories.is_empty());
        if sends_none
            && !filed.is_empty()
            && let Some(with) = ics::with_categories(&body, &filed)
        {
            body = with;
        }
        if ics::ends_what_its_server_schedules(&body, &held) {
            return Err(WOULD_CALL_IT_OFF.to_owned());
        }
        self.store_invitation(
            signed,
            (&client, calendar, url.as_str()),
            body,
            Precondition::IfMatch(&version),
        )
    }

    /// Write `body`, an invitation as its calendar keeps it, at `url` of `calendar`, over what
    /// `precondition` names, and keep what the server holds now.
    fn store_invitation(
        &mut self,
        signed: &CalendarAccount,
        (client, calendar, url): (&DavClient, &str, &str),
        body: String,
        precondition: Precondition<'_>,
    ) -> Result<Written, String> {
        let entry = Self::entry_of(&body, calendar, url);
        let response = client
            .put(url, body.into_bytes(), Some(ENTRY_TYPE), precondition)
            .map_err(|error| {
                if is_unreached(&error) {
                    UNREACHED.to_owned()
                } else {
                    format!("the calendar server did not answer: {error}")
                }
            })?;
        match response.status().as_u16() {
            412 => Err("This invitation changed in that calendar meanwhile".to_owned()),
            _ if response.status().is_success() => {
                if let Some(entry) = entry {
                    let etag = response.etag().unwrap_or_default();
                    if let Some(held) = self.held.get_mut(&signed.id) {
                        held.entries.insert(
                            (calendar.to_owned(), url.to_owned()),
                            HeldEntry { etag, entry },
                        );
                    }
                }
                Ok(Written::Stored(url.to_owned()))
            }
            status => Err(format!("the calendar server answered {status}")),
        }
    }

    /// The version the server last gave the entry at `href`, when it gave one.
    fn version(&self, account: &str, href: &str) -> Option<String> {
        self.held
            .get(account)?
            .entries
            .iter()
            .find(|((_, kept), _)| kept == href)
            .map(|(_, held)| held.etag.clone())
    }

    fn remember(&mut self, signed: &CalendarAccount, entry: &Entry, href: &str, etag: &str) {
        let held = self.held.entry(signed.id.clone()).or_default();
        let mut entry = entry.clone();
        href.clone_into(&mut entry.href);
        etag.clone_into(&mut entry.etag);
        held.entries.insert(
            (entry.calendar.clone(), href.to_owned()),
            HeldEntry {
                etag: etag.to_owned(),
                entry,
            },
        );
    }

    fn forget(&mut self, account: &str, href: &str) {
        if let Some(held) = self.held.get_mut(account) {
            held.entries.retain(|(_, kept), _| kept != href);
        }
    }

    /// A connection that signs in to `signed`'s server and follows no redirect, so the sign-in
    /// reaches that server alone.
    fn client(signed: &CalendarAccount) -> Result<(DavClient, Url), String> {
        let server = Url::parse(signed.server.trim())
            .map_err(|_| "The calendar server's address does not read".to_owned())?;
        if !server.username().is_empty() || server.password().is_some() {
            return Err("The server's address must not carry a sign-in".to_owned());
        }
        let credentials = Credentials::new(signed.username.clone(), signed.password.clone());
        let client = DavClient::new(&Options {
            user_agent: None,
            timeout: std::time::Duration::from_secs(20),
            connect_timeout: Some(std::time::Duration::from_secs(8)),
            redirects: Redirects::Refuse,
            lookup: None,
        })
        .map_err(|_| "could not start the secure connection".to_owned())?
        .with_credentials(credentials);
        Ok((client, server))
    }
}

/// What a write came to.
enum Written {
    /// The server keeps it at this address now.
    Stored(String),
    Removed,
}

/// The most characters a calendar's name is shown in.
const MAX_CALENDAR_CHARS: usize = 80;

/// A calendar's name as its server says it, shown as a name: its words on one line, whatever
/// breaks, tabs and runs of space the server wrote into it, and cut where it is long.
fn shown_name(name: &str) -> String {
    name.split(|character: char| character.is_control() || character.is_whitespace())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_CALENDAR_CHARS)
        .collect()
}

/// Why a server was not reached at all, as opposed to refusing what it was asked.
const UNREACHED: &str = "the calendar server could not be reached";

fn is_unreached(error: &tilo_dav::client::Error) -> bool {
    matches!(error, tilo_dav::client::Error::Transport(_))
}

/// Why what a server holds of an entry was not taken as read.
enum Unread {
    /// It is gone.
    Gone,
    /// It is no longer the version this calendar last saw.
    Changed,
    /// It could not be read: why, for the person.
    Failed(String),
}

/// What the server holds at `url` now, and its version. It is read before an entry is written
/// over it, because what each line says of who writes to the people it names, and whom it names
/// that an entry here cannot hold, have to stay as the server holds them (RFC 6638, 3.2.1.2).
/// `known` is the version this calendar last saw there: a server that holds another one changed
/// the entry since, and nothing is written over it.
fn held_now(
    client: &DavClient,
    url: &Url,
    known: Option<&str>,
) -> Result<(String, String), Unread> {
    let unread = |why: &str| {
        Unread::Failed(format!(
            "the calendar server did not show what it holds of this event ({why}), so nothing \
             was written to it"
        ))
    };
    let response = client
        .get(url.clone())
        .map_err(|error| unread(&error.to_string()))?;
    if response.status().as_u16() == 404 {
        return Err(Unread::Gone);
    }
    if !response.status().is_success() {
        return Err(unread(&format!(
            "server returned {}",
            response.status().as_u16()
        )));
    }
    let Some(read) = response.etag() else {
        return Err(unread("it gave no version of it"));
    };
    if known.is_some_and(|known| !same_version(known, &read)) {
        return Err(Unread::Changed);
    }
    let version = if let Some(known) = known
        && !is_strong(&read)
        && is_strong(known)
    {
        known.to_owned()
    } else {
        read
    };
    let bytes = response
        .read_bounded(MAX_ENTRY_BYTES)
        .map_err(|error| unread(&error.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|_| unread("it is not text"))?;
    Ok((text, version))
}

/// Delete the entry at `url`, telling its server to answer nobody for its person (RFC 6638,
/// 8.1).
fn delete(client: &DavClient, url: &str) -> Result<(), String> {
    let response = client
        .send(
            "DELETE",
            url,
            &[("Schedule-Reply", "F")],
            tilo_dav::Body::Empty,
            Precondition::IfMatch("*"),
        )
        .map_err(|error| {
            if is_unreached(&error) {
                UNREACHED.to_owned()
            } else {
                format!("the calendar server did not answer: {error}")
            }
        })?;
    if response.status().is_success() || response.status().as_u16() == 404 {
        return Ok(());
    }
    Err(format!(
        "the calendar server answered {}",
        response.status().as_u16()
    ))
}

/// The opaque tag of the entity-tag `tag`, without the mark of a weak one (RFC 7232, 2.3).
fn opaque_tag(tag: &str) -> &str {
    let tag = tag.trim();
    let tag = tag.strip_prefix("W/").unwrap_or(tag);
    tag.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(tag)
}

/// Whether `tag` is a strong entity-tag, the only kind a write names the version it goes over by
/// (RFC 7232, 3.1).
fn is_strong(tag: &str) -> bool {
    let tag = tag.trim();
    tag.len() >= 2 && tag.starts_with('"') && tag.ends_with('"')
}

/// Whether two entity-tags name one version of an entry (RFC 7232, 2.3.2).
fn same_version(known: &str, read: &str) -> bool {
    opaque_tag(known) == opaque_tag(read)
}

/// The `UID` the entry `text` goes by: its first, since the parts of one entry go by one.
fn uid_of(text: &str) -> Option<String> {
    text.replace("\r\n ", "").lines().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        property
            .split(';')
            .next()
            .is_some_and(|property| property.eq_ignore_ascii_case("UID"))
            .then(|| value.trim().to_owned())
    })
}

/// The entry `text` with the `UID` every part of it goes by replaced by `uid`.
fn set_uid(text: &str, uid: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive("\r\n") {
        let property = line.split([';', ':']).next().unwrap_or_default();
        if property.eq_ignore_ascii_case("UID") {
            out.push_str("UID:");
            out.push_str(uid);
            out.push_str("\r\n");
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Whether the entry `text` names somebody a calendar server could write to: who organises it, or
/// somebody it invites.
fn names_somebody(text: &str) -> bool {
    let named = ics::named(text);
    !(named.organizers.is_empty() && named.attendees.is_empty())
}

/// The entry `text` with `attendee`'s own answer changed to `answer` (`accepted`, `declined` or
/// `tentative`), its line otherwise as its server holds it, so the server tells the organiser
/// what its person answered.
fn with_answer(text: &str, attendee: &str, answer: &str) -> Option<String> {
    let status = match answer {
        "accepted" => "ACCEPTED",
        "declined" => "DECLINED",
        "tentative" => "TENTATIVE",
        _ => "NEEDS-ACTION",
    };
    let wanted = attendee.trim().to_ascii_lowercase();
    let unfolded = text.replace("\r\n ", "");
    let mut out = String::with_capacity(unfolded.len());
    let mut found = false;
    for line in unfolded.lines() {
        let property = line.split([';', ':']).next().unwrap_or_default();
        let names = property.eq_ignore_ascii_case("ATTENDEE")
            && line
                .split_once(':')
                .is_some_and(|(_, value)| value.trim().to_ascii_lowercase().contains(&wanted));
        if !names {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        found = true;
        out.push_str(&with_partstat(line, status));
        out.push_str("\r\n");
    }
    found.then_some(out)
}

/// The line `line` with its `PARTSTAT` set to `status`, every other parameter as it is.
fn with_partstat(line: &str, status: &str) -> String {
    let (head, value) = line.split_once(':').unwrap_or((line, ""));
    let mut kept = String::new();
    if let Some((property, parameters)) = head.split_once(';') {
        kept.push_str(property);
        for parameter in split_parameters(parameters) {
            if parameter
                .split_once('=')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("PARTSTAT"))
            {
                continue;
            }
            kept.push(';');
            kept.push_str(&parameter);
        }
    } else {
        kept.push_str(head);
    }
    format!("{kept};PARTSTAT={status}:{value}")
}

/// The parameters of a content line's head after its property, one by one, a quoted value's `;`
/// and `:` kept within it.
fn split_parameters(parameters: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for letter in parameters.chars() {
        match letter {
            '"' => {
                quoted = !quoted;
                current.push(letter);
            }
            ';' if !quoted => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(letter),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The event a server's entry is, as its calendar's record holds it.
fn entry_of(
    event: &Event,
    notes: &str,
    uid: Option<String>,
    calendar: &str,
    href: &str,
    text: &str,
) -> Entry {
    let zone = event.timezone.as_str();
    Entry {
        uid: uid.unwrap_or_else(|| event.id.to_string()),
        href: href.to_owned(),
        etag: String::new(),
        calendar: calendar.to_owned(),
        title: event.title.clone(),
        start: wall_text(event.start),
        end: wall_text(event.end),
        all_day: event.all_day,
        timezone: if zone.is_empty() {
            "floating".to_owned()
        } else {
            zone.to_owned()
        },
        location: event.location.clone(),
        recurrence: recurrence_text(event),
        recurrence_weekdays: event
            .recurrence_weekdays
            .iter()
            .map(|day| day.trim().to_ascii_uppercase())
            .collect(),
        recurrence_every: event.recurrence_interval.max(1),
        recurrence_until: event
            .recurrence_until
            .map(|date| date.to_string())
            .unwrap_or_default(),
        recurrence_count: event.recurrence_count.unwrap_or(0),
        exdates: event
            .recurrence_exdates
            .iter()
            .map(ToString::to_string)
            .collect(),
        overrides: event
            .occurrence_overrides
            .iter()
            .map(|edit| Occurrence {
                date: edit.original_date.to_string(),
                time: edit
                    .original_time
                    .map(|time| time.to_string())
                    .unwrap_or_default(),
                cancelled: edit.cancelled,
                start: edit.start.map(wall_text).unwrap_or_default(),
                end: edit.end.map(wall_text).unwrap_or_default(),
                title: edit.title.clone().unwrap_or_default(),
                location: edit.location.clone().unwrap_or_default(),
            })
            .collect(),
        organizer: event
            .organizer
            .and_then(|id| event.attendees.iter().find(|attendee| attendee.id == id))
            .map(|organizer| organizer.email.clone())
            .unwrap_or_default(),
        attendees: event
            .attendees
            .iter()
            .map(|attendee| Guest {
                name: attendee.name.clone(),
                email: attendee.email.clone(),
                status: status_of(attendee.status),
                role: "required".to_owned(),
            })
            .collect(),
        categories: event.categories.iter().cloned().collect(),
        color: color_of(text),
        notes: notes.to_owned(),
        remind: event
            .reminder_min
            .and_then(|minutes| {
                let at = instant_start(zone, event.start)?
                    .checked_sub(jiff::SignedDuration::from_mins(minutes).as_secs())?;
                Some(jiff::Timestamp::from_second(at).ok()?.to_string())
            })
            .unwrap_or_default(),
    }
}

/// The instant the wall time `wall` is in `zone`.
fn instant_start(zone: &str, wall: DateTime) -> Option<i64> {
    Some(match zone {
        "" | "floating" => wall
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()?
            .timestamp()
            .as_second(),
        name => jiff::tz::TimeZone::get(name)
            .ok()
            .and_then(|timezone| wall.to_zoned(timezone).ok())
            .map(|zoned| zoned.timestamp().as_second())?,
    })
}

/// The minutes before its start the entry's reminder rings, where it rings.
fn reminder_min(entry: &Entry) -> Option<i64> {
    if entry.remind.is_empty() {
        return None;
    }
    let remind = entry.remind.parse::<jiff::Timestamp>().ok()?.as_second();
    let start = instant_start(&entry.timezone, wall_of_text(&entry.start)?)?;
    let minutes = start.checked_sub(remind)?.checked_div(60)?;
    (minutes > 0).then_some(minutes)
}

/// The wall time `wall` is, as iCalendar writes one (`2026-11-19T19:00:00`).
fn wall_text(wall: DateTime) -> String {
    wall.strftime("%Y-%m-%dT%H:%M:%S").to_string()
}

/// The wall time `text` names, `2026-11-19T19:00:00`.
fn wall_of_text(text: &str) -> Option<DateTime> {
    DateTime::strptime("%Y-%m-%dT%H:%M:%S", text.trim()).ok()
}

/// The entry's repeat as its rule's frequency and days (`FREQ=WEEKLY;BYDAY=MO`), empty for one
/// occurrence. Its interval, last day and count are fields of their own.
fn recurrence_text(event: &Event) -> String {
    let days = crate::model::rule_weekdays(&event.recurrence_weekdays);
    if days.is_empty() {
        return event.recurrence.to_rrule();
    }
    if matches!(event.recurrence, Recurrence::Weekly | Recurrence::Weekdays) {
        format!("FREQ=WEEKLY;BYDAY={}", days.join(","))
    } else {
        event.recurrence.to_rrule()
    }
}

/// The repeat an `RRULE` names: its frequency, interval, last day, count and the days it comes
/// on.
fn recurrence_parts(rule: &str) -> (Recurrence, u32, Option<Date>, Option<u32>, Vec<String>) {
    let mut recurrence = Recurrence::None;
    let mut interval = 1;
    let mut until = None;
    let mut count = None;
    let mut days: Vec<String> = Vec::new();
    for part in rule.split(';') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        match name.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                recurrence = match value.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Recurrence::Daily,
                    "WEEKLY" => Recurrence::Weekly,
                    "MONTHLY" => Recurrence::Monthly,
                    "YEARLY" => Recurrence::Yearly,
                    _ => Recurrence::None,
                };
            }
            "BYDAY" => {
                days = value
                    .split(',')
                    .filter_map(crate::model::weekday_of)
                    .map(|day| crate::model::weekday_code(day).to_owned())
                    .collect();
                // Every weekday, as the app's own preset names it.
                if recurrence == Recurrence::Weekly && days == ["MO", "TU", "WE", "TH", "FR"] {
                    recurrence = Recurrence::Weekdays;
                }
            }
            "INTERVAL" => interval = value.trim().parse().unwrap_or(1),
            "UNTIL" => until = date_of(value.trim()),
            "COUNT" => count = value.trim().parse().ok(),
            _ => {}
        }
    }
    (recurrence, interval.max(1), until, count, days)
}

/// The day a written `UNTIL` names, `YYYYMMDD` or `YYYYMMDDTHHMMSSZ`.
fn date_of(value: &str) -> Option<Date> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let year = digits.get(0..4)?.parse().ok()?;
    let month = digits.get(4..6)?.parse().ok()?;
    let day = digits.get(6..8)?.parse().ok()?;
    Date::new(year, month, day).ok()
}

/// The colour the entry `text` says it is, its `COLOR`.
fn color_of(text: &str) -> String {
    text.replace("\r\n ", "")
        .lines()
        .find_map(|line| {
            let (property, value) = line.split_once(':')?;
            property
                .split(';')
                .next()
                .is_some_and(|property| property.eq_ignore_ascii_case("COLOR"))
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

/// The entry `text` with its `COLOR` set to `color` in every part of it, or none where it is
/// empty.
fn with_color(text: &str, color: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive("\r\n") {
        let property = line.split([';', ':']).next().unwrap_or_default();
        if property.eq_ignore_ascii_case("COLOR") {
            continue;
        }
        if property.eq_ignore_ascii_case("END")
            && line.trim().eq_ignore_ascii_case("END:VEVENT")
            && !color.is_empty()
        {
            out.push_str("COLOR:");
            out.push_str(color);
            out.push_str("\r\n");
        }
        out.push_str(line);
    }
    out
}

fn status_of(status: InviteStatus) -> String {
    match status {
        InviteStatus::NeedsAction => "needs-action",
        InviteStatus::Accepted => "accepted",
        InviteStatus::Tentative => "tentative",
        InviteStatus::Declined => "declined",
    }
    .to_owned()
}

fn invite_status_of(status: &str) -> InviteStatus {
    match status {
        "accepted" => InviteStatus::Accepted,
        "declined" => InviteStatus::Declined,
        "tentative" => InviteStatus::Tentative,
        _ => InviteStatus::NeedsAction,
    }
}

/// The iCalendar text the entry is written as.
pub fn text_of(entry: &Entry) -> String {
    let zone = entry.timezone.as_str();
    let start = wall_of_text(&entry.start).unwrap_or_default();
    let end = wall_of_text(&entry.end).unwrap_or_default();
    let (recurrence, _, _, _, weekdays) = recurrence_parts(&entry.recurrence);
    let mut event = Event {
        id: Id::new_v4(),
        title: entry.title.clone(),
        start,
        end,
        all_day: entry.all_day,
        location: entry.location.clone(),
        recurrence,
        // The days stand in their own field, so a rule kept as its frequency alone still shows
        // them; one written whole says its own.
        recurrence_weekdays: if entry.recurrence_weekdays.is_empty() {
            weekdays.into_iter().collect()
        } else {
            entry.recurrence_weekdays.iter().cloned().collect()
        },
        recurrence_interval: entry.recurrence_every.max(1),
        recurrence_until: entry.recurrence_until.parse::<Date>().ok(),
        recurrence_count: (entry.recurrence_count > 0).then_some(entry.recurrence_count),
        timezone: match zone {
            "" => "floating".to_owned(),
            zone => zone.to_owned(),
        },
        ..Event::default()
    };
    event.reminder_min = reminder_min(entry);
    event.recurrence_exdates = entry
        .exdates
        .iter()
        .filter_map(|date| date.parse::<Date>().ok())
        .collect();
    event.occurrence_overrides = entry
        .overrides
        .iter()
        .filter_map(|edit| {
            let original_date = edit.date.parse::<Date>().ok()?;
            Some(OccurrenceOverride {
                original_date,
                original_time: edit.time.parse::<Time>().ok(),
                cancelled: edit.cancelled,
                start: (!edit.start.is_empty())
                    .then(|| wall_of_text(&edit.start))
                    .flatten(),
                end: (!edit.end.is_empty())
                    .then(|| wall_of_text(&edit.end))
                    .flatten(),
                end_later_fold: false,
                title: (!edit.title.is_empty()).then(|| edit.title.clone()),
                location: (!edit.location.is_empty()).then(|| edit.location.clone()),
            })
        })
        .collect();
    event.categories = entry.categories.iter().cloned().collect();
    let mut attendees: Vec<Attendee> = entry
        .attendees
        .iter()
        .map(|guest| Attendee {
            id: Id::new_v4(),
            name: guest.name.clone(),
            email: guest.email.clone(),
            status: invite_status_of(&guest.status),
        })
        .collect();
    if !entry.organizer.is_empty() {
        let found = attendees
            .iter()
            .find(|attendee| attendee.email.eq_ignore_ascii_case(&entry.organizer))
            .map(|attendee| attendee.id);
        let organizer = found.unwrap_or_else(|| {
            let id = Id::new_v4();
            attendees.push(Attendee {
                id,
                name: String::new(),
                email: entry.organizer.clone(),
                status: InviteStatus::NeedsAction,
            });
            id
        });
        event.organizer = Some(organizer);
    }
    event.attendees = attendees.into_iter().collect();
    let mut calendar = Calendar::default();
    calendar.events.push(event.clone());
    if !entry.notes.trim().is_empty() {
        calendar.notes.insert(
            event.id.to_string(),
            tilo_document::RichText::from_markdown(entry.notes.clone()),
        );
    }
    with_color(&set_uid(&ics::export(&calendar), &entry.uid), &entry.color)
}
