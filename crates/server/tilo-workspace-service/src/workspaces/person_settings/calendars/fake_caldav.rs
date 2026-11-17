//! A small `CalDAV` server for tests, laid out like the servers people keep
//! themselves.
//!
//! It speaks enough of RFC 4791 and RFC 6578 for a client's whole life with
//! an account: discovery from the address a person types (the principal,
//! its calendar home, the calendars in it with what the person may do
//! there), a calendar's sync token, `sync-collection` reports from a token
//! and from none, a `calendar-query` for a server asked without one, and
//! `GET`, `PUT` and `DELETE` of one entry with their `ETag` preconditions.
//! Every request must sign in with the account's name and password, as
//! Basic credentials.
//!
//! It schedules by itself, as the servers people rent do (RFC 6638): an
//! account whose name is an address is the person written to there. What an
//! entry that person organises makes the server send is read from the
//! standard's tables, line by line: who writes to somebody a line names
//! (`SCHEDULE-AGENT`, section 7.1), in the entry as it was and as it is now,
//! decides between an invitation, a cancellation and nothing (3.2.1.1 for a
//! new entry, 3.2.1.2 for a changed one, 3.2.1.3 for a removed one). Where
//! somebody else organises, the organiser is answered when the person's own
//! answer changes, when they leave an occurrence out and when the entry goes
//! (3.2.2.3, 3.2.2.4), unless the removal says not to (8.1). Nothing is sent
//! anywhere: the server keeps whom it would have written to
//! ([`Server::mailed`]).
//!
//! What the server schedules is an entry every part of which, the event and
//! each occurrence that stands apart, says who organises it (3.1). Somebody
//! is named by any address, a mail address or another (RFC 5545, 3.3.3).
//!
//! An entry is known by its `UID`. Written over by one under another `UID`,
//! it is refused (RFC 4791, 5.3.2.1, `no-uid-conflict`). A server that does
//! not keep that rule ([`Server::takes_another_uid`]) reads what it is given
//! as another event: the one it held is removed and the one it holds now is
//! new, each by its table.
//!
//! A test changes a calendar as another client would ([`Server::put`],
//! [`Server::remove`]), and reads what a client left on it
//! ([`Server::held`]) and everything it was asked ([`Server::requests`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use test_http::{Reply, Request, Responder};

const XML: &str = "application/xml; charset=utf-8";
const NAMESPACES: &str = "xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"";

/// One entry a calendar holds, and the version the server gave it.
struct Kept {
    etag: u64,
    ics: String,
}

#[derive(Default)]
struct Calendar {
    name: String,
    read_only: bool,
    entries: BTreeMap<String, Kept>,
    /// Each change the calendar took: the token it led to, and the entry.
    changes: Vec<(u64, String)>,
}

struct State {
    user: String,
    password: String,
    calendars: BTreeMap<String, Calendar>,
    /// Grows with every change to any calendar.
    token: u64,
    /// Whether `sync-collection` is answered: a server without it says 501.
    syncs: bool,
    /// Everyone the server wrote to by itself, in order: what it sent and
    /// the address it went to.
    mailed: Vec<String>,
    /// Whether an entry is written over by one under another `UID`: a server
    /// that keeps RFC 4791's `no-uid-conflict` says no.
    any_uid: bool,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn sync_token(token: u64) -> String {
    format!("urn:test:sync:{token}")
}

fn etag(version: u64) -> String {
    format!("\"{version}\"")
}

/// Standard base64 with padding, as Basic credentials are written.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let letter = |index: u32| {
        usize::try_from(index & 63)
            .ok()
            .and_then(|index| ALPHABET.get(index))
            .map_or('=', |byte| char::from(*byte))
    };
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let byte = |place: usize| u32::from(chunk.get(place).copied().unwrap_or(0));
        let group = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        out.push(letter(group >> 18));
        out.push(letter(group >> 12));
        out.push(if chunk.len() > 1 {
            letter(group >> 6)
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { letter(group) } else { '=' });
    }
    out
}

fn multistatus(responses: &str, token: Option<u64>) -> Reply {
    let mut body =
        format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus {NAMESPACES}>");
    body.push_str(responses);
    if let Some(token) = token {
        let _ = write!(body, "<d:sync-token>{}</d:sync-token>", sync_token(token));
    }
    body.push_str("</d:multistatus>");
    Reply::status(207).header("Content-Type", XML).body(body)
}

fn found(href: &str, props: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

fn gone(href: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
    )
}

/// Who is to write to whoever a line names: what its `SCHEDULE-AGENT` says
/// (RFC 6638, 7.1). A line that says nothing leaves it to the server, and a
/// value the server does not know reads as `NONE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Writes {
    Server,
    Client,
    Nobody,
}

/// One line of an entry that names somebody.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Named {
    address: String,
    writes: Writes,
    /// Whether they come, as the line says it (`PARTSTAT`).
    coming: String,
}

/// What of an entry a server that schedules reads.
#[derive(Default)]
struct Read {
    organizer: Option<Named>,
    /// Everyone it invites, once each.
    invited: Vec<Named>,
    /// How many occurrences it leaves out (`EXDATE`).
    left_out: usize,
}

/// What the parameter `name` says in the upper-cased line `upper`.
fn param<'a>(upper: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(";{name}=");
    let at = upper.find(&key)?;
    let rest = upper.get(at.saturating_add(key.len())..)?;
    Some(
        rest.split([';', ':'])
            .next()
            .unwrap_or(rest)
            .trim_matches('"'),
    )
}

/// Whom the line `line` names: what it says after its name and parameters,
/// which is all that follows the first colon no quoted parameter holds,
/// without its `mailto:` and in lower case. Somebody may be named by another
/// address than a mail address (RFC 5545, 3.3.3), and is written to there.
fn address_named(line: &str) -> Option<String> {
    let mut quoted = false;
    let value = line.char_indices().find_map(|(at, letter)| match letter {
        '"' => {
            quoted = !quoted;
            None
        }
        ':' if !quoted => line.get(at.saturating_add(1)..),
        _ => None,
    })?;
    let value = value.trim();
    let address = value
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .and_then(|_| value.get(7..))
        .unwrap_or(value)
        .trim();
    (!address.is_empty()).then(|| address.to_ascii_lowercase())
}

/// What the entry `ics` says to a server that schedules.
fn read(ics: &str) -> Read {
    let mut read = Read::default();
    // How many events and occurrences the entry holds, and who each of them
    // says organises it.
    let mut parts = 0_usize;
    let mut organised: Vec<String> = Vec::new();
    for line in ics.replace("\r\n ", "").lines() {
        let upper = line.to_ascii_uppercase();
        if upper.trim() == "BEGIN:VEVENT" {
            parts = parts.saturating_add(1);
            continue;
        }
        let property = upper.split([';', ':']).next().unwrap_or_default();
        if property == "EXDATE" {
            let dates = upper.rsplit(':').next().unwrap_or_default();
            read.left_out = read.left_out.saturating_add(dates.split(',').count());
            continue;
        }
        if property != "ORGANIZER" && property != "ATTENDEE" {
            continue;
        }
        let Some(address) = address_named(line) else {
            continue;
        };
        let writes = match param(&upper, "SCHEDULE-AGENT") {
            None | Some("SERVER") => Writes::Server,
            Some("CLIENT") => Writes::Client,
            Some(_) => Writes::Nobody,
        };
        let named = Named {
            address,
            writes,
            coming: param(&upper, "PARTSTAT")
                .unwrap_or("NEEDS-ACTION")
                .to_owned(),
        };
        match property {
            "ORGANIZER" => {
                organised.push(named.address.clone());
                read.organizer = Some(named);
            }
            "ATTENDEE"
                if !read
                    .invited
                    .iter()
                    .any(|other| other.address == named.address) =>
            {
                read.invited.push(named);
            }
            _ => {}
        }
    }
    // RFC 6638, 3.1: the server schedules an entry where every one of its
    // parts says who organises it, and all of them say the same.
    let throughout =
        organised.len() == parts && organised.iter().all(|by| Some(by) == organised.first());
    if !throughout {
        read.organizer = None;
    }
    read
}

/// The `UID` the entry `ics` goes by: its first, since the parts of one entry
/// go by one.
fn uid_it_goes_by(ics: &str) -> Option<String> {
    ics.replace("\r\n ", "").lines().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let property = property.split(';').next().unwrap_or(property);
        property
            .eq_ignore_ascii_case("UID")
            .then(|| value.trim().to_owned())
    })
}

/// What RFC 6638's tables have a server send somebody an entry's organiser
/// invites, by who wrote to them in the entry as it was (`None`: it did not
/// name them, or there was no entry) and in the entry as it is now (`None`:
/// it no longer names them, or the entry is gone). Section 3.2.1.2 is the
/// whole table, for a changed entry; 3.2.1.1 is its row `<Absent>`, for a
/// new one; 3.2.1.3 is its column `<Removed>`, for a removed one. A line
/// left to the server invites, whatever stood before it; a line the server
/// wrote for calls off, whatever else comes after it; nothing else sends.
fn by_the_table(original: Option<Writes>, modified: Option<Writes>) -> Option<&'static str> {
    match (original, modified) {
        (_, Some(Writes::Server)) => Some("REQUEST"),
        (Some(Writes::Server), _) => Some("CANCEL"),
        _ => None,
    }
}

/// What a calendar server that schedules by itself sends, and to whom, when
/// the entry `old` becomes `new` in the calendar of the person it writes to
/// at `account`.
///
/// Where that person organises the entry, before or after, each person it
/// invites is sent what [`by_the_table`] gives for their line. An entry they
/// did not organise counts as no entry (3.2.3.1: an entry that scheduled
/// nothing and now does is created, and one that did and no longer does is
/// removed), and the organiser is sent nothing where they are invited too
/// (3.2.1).
///
/// Where somebody else organises and leaves answering to the server, the
/// organiser is told when the person's own answer changes or they leave an
/// occurrence out (3.2.2.3), and when the entry goes (3.2.2.4), unless the
/// request said not to answer (`reply`, section 8.1).
fn sent(account: Option<&str>, old: Option<&str>, new: Option<&str>, reply: bool) -> Vec<String> {
    let Some(account) = account else {
        return Vec::new();
    };
    let was = old.map(read).unwrap_or_default();
    let now = new.map(read).unwrap_or_default();
    let theirs = |read: &Read| {
        read.organizer
            .as_ref()
            .is_some_and(|by| by.address == account)
    };
    let mut sent = Vec::new();
    if theirs(&was) || theirs(&now) {
        let before: &[Named] = if theirs(&was) { &was.invited } else { &[] };
        let after: &[Named] = if theirs(&now) { &now.invited } else { &[] };
        let writes = |lines: &[Named], address: &str| {
            lines
                .iter()
                .find(|named| named.address == address)
                .map(|named| named.writes)
        };
        let gone = before
            .iter()
            .filter(|named| writes(after, &named.address).is_none());
        for named in after.iter().chain(gone) {
            if named.address == account {
                continue;
            }
            let original = writes(before, &named.address);
            let modified = writes(after, &named.address);
            if let Some(method) = by_the_table(original, modified) {
                sent.push(format!("{method} to {}", named.address));
            }
        }
        return sent;
    }
    let answered = |read: &Read| {
        read.organizer
            .as_ref()
            .filter(|by| by.writes == Writes::Server)
            .map(|by| by.address.clone())
    };
    let own = |read: &Read| {
        read.invited
            .iter()
            .find(|named| named.address == account)
            .map(|named| named.coming.clone())
    };
    let (Some(organizer), Some(came)) = (answered(&was), own(&was)) else {
        return sent;
    };
    let answers = match new {
        None => reply,
        Some(_) => {
            answered(&now).is_some()
                && (own(&now).is_some_and(|comes| comes != came) || now.left_out > was.left_out)
        }
    };
    if answers {
        sent.push(format!("REPLY to {organizer}"));
    }
    sent
}

impl State {
    /// What a server knows that has one account, `user` with `password`,
    /// and holds no calendar yet.
    fn new(user: &str, password: &str) -> Self {
        Self {
            user: user.to_owned(),
            password: password.to_owned(),
            calendars: BTreeMap::new(),
            token: 1,
            syncs: true,
            mailed: Vec::new(),
            any_uid: false,
        }
    }

    /// The account's name in an address of this server: what stands before
    /// the `@` where the account signs in with an address.
    fn slug(&self) -> &str {
        self.user.split('@').next().unwrap_or(&self.user)
    }

    /// The address the account's person is written to at: the account's
    /// name, where that is an address.
    fn address(&self) -> Option<String> {
        self.user
            .contains('@')
            .then(|| self.user.to_ascii_lowercase())
    }

    fn principal(&self) -> String {
        format!("/dav/principals/{}/", self.slug())
    }

    fn home(&self) -> String {
        format!("/dav/calendars/{}/", self.slug())
    }

    /// The calendar and the entry a path under the home names; the entry is
    /// empty for the calendar itself.
    fn place(&self, path: &str) -> Option<(String, String)> {
        let rest = path.strip_prefix(&self.home())?;
        let (calendar, entry) = rest.split_once('/').unwrap_or((rest, ""));
        (!calendar.is_empty()).then(|| (calendar.to_owned(), entry.to_owned()))
    }

    fn signed_in(&self, request: &Request) -> bool {
        let wanted = base64(format!("{}:{}", self.user, self.password).as_bytes());
        request
            .header("Authorization")
            .and_then(|sent| sent.split_once(' '))
            .is_some_and(|(scheme, sent)| {
                scheme.eq_ignore_ascii_case("Basic") && sent.trim() == wanted
            })
    }

    fn calendar_props(&self, calendar: &Calendar) -> String {
        let write = if calendar.read_only {
            ""
        } else {
            "<d:privilege><d:write/></d:privilege>"
        };
        format!(
            "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
             <d:displayname>{}</d:displayname>\
             <d:current-user-privilege-set><d:privilege><d:read/></d:privilege>{write}\
             </d:current-user-privilege-set><d:sync-token>{}</d:sync-token>",
            escaped(&calendar.name),
            sync_token(self.token)
        )
    }

    fn propfind(&self, path: &str, deep: bool) -> Reply {
        let home = self.home();
        let principal = format!(
            "<d:current-user-principal><d:href>{}</d:href></d:current-user-principal>",
            self.principal()
        );
        if path == home || format!("{path}/") == home {
            let mut responses = found(&home, "<d:resourcetype><d:collection/></d:resourcetype>");
            if deep {
                for (slug, calendar) in &self.calendars {
                    responses.push_str(&found(
                        &format!("{home}{slug}/"),
                        &self.calendar_props(calendar),
                    ));
                }
            }
            return multistatus(&responses, None);
        }
        if let Some((slug, entry)) = self.place(path) {
            return match self.calendars.get(&slug).filter(|_| entry.is_empty()) {
                Some(calendar) => multistatus(&found(path, &self.calendar_props(calendar)), None),
                None => Reply::status(404),
            };
        }
        if path == self.principal() {
            let props = format!(
                "{principal}<c:calendar-home-set><d:href>{home}</d:href></c:calendar-home-set>"
            );
            return multistatus(&found(path, &props), None);
        }
        if path.starts_with("/dav") {
            let props = format!("{principal}<d:resourcetype><d:collection/></d:resourcetype>");
            return multistatus(&found(path, &props), None);
        }
        Reply::status(404)
    }

    fn entry_response(href: &str, kept: &Kept) -> String {
        found(
            href,
            &format!(
                "<d:getetag>{}</d:getetag><c:calendar-data>{}</c:calendar-data>",
                escaped(&etag(kept.etag)),
                escaped(&kept.ics)
            ),
        )
    }

    fn report(&self, path: &str, body: &str) -> Reply {
        let Some(calendar) = self
            .place(path)
            .filter(|(_, entry)| entry.is_empty())
            .and_then(|(slug, _)| self.calendars.get(&slug))
        else {
            return Reply::status(404);
        };
        let collection = format!("{}/", path.trim_end_matches('/'));
        let everything = || {
            calendar
                .entries
                .iter()
                .map(|(name, kept)| Self::entry_response(&format!("{collection}{name}"), kept))
                .collect::<String>()
        };
        if body.contains("calendar-query") {
            return multistatus(&everything(), None);
        }
        if !body.contains("sync-collection") {
            return Reply::status(400);
        }
        if !self.syncs {
            return Reply::status(501);
        }
        let asked = body
            .split_once("sync-token>")
            .and_then(|(_, rest)| rest.split_once('<'))
            .map_or("", |(token, _)| token.trim());
        if asked.is_empty() {
            return multistatus(&everything(), Some(self.token));
        }
        let since = match asked.strip_prefix("urn:test:sync:").map(str::parse::<u64>) {
            Some(Ok(since)) if since <= self.token => since,
            // A token this server never gave: its client reads everything again.
            _ => {
                return Reply::status(403).header("Content-Type", XML).body(format!(
                    "<d:error {NAMESPACES}><d:valid-sync-token/></d:error>"
                ));
            }
        };
        let mut changed: Vec<&str> = calendar
            .changes
            .iter()
            .filter(|(token, _)| *token > since)
            .map(|(_, name)| name.as_str())
            .collect();
        changed.sort_unstable();
        changed.dedup();
        let responses: String = changed
            .into_iter()
            .map(|name| {
                let href = format!("{collection}{name}");
                calendar
                    .entries
                    .get(name)
                    .map_or_else(|| gone(&href), |kept| Self::entry_response(&href, kept))
            })
            .collect();
        multistatus(&responses, Some(self.token))
    }

    /// Whether `request`'s preconditions hold against what `held` is.
    fn allowed(request: &Request, held: Option<&Kept>) -> bool {
        let none_match = request.header("If-None-Match").map(str::trim);
        let matches = request.header("If-Match").map(str::trim);
        !(none_match == Some("*") && held.is_some())
            && matches.is_none_or(|wanted| {
                held.is_some_and(|kept| wanted == "*" || wanted == etag(kept.etag))
            })
    }

    fn put(&mut self, path: &str, request: &Request) -> Reply {
        let Some((slug, name)) = self.place(path).filter(|(_, entry)| !entry.is_empty()) else {
            return Reply::status(404);
        };
        let next = self.token.saturating_add(1);
        let account = self.address();
        let any_uid = self.any_uid;
        let Some(calendar) = self.calendars.get_mut(&slug) else {
            return Reply::status(404);
        };
        if calendar.read_only {
            return Reply::status(403);
        }
        if !Self::allowed(request, calendar.entries.get(&name)) {
            return Reply::status(412);
        }
        let text = request.body_text();
        // RFC 4791, 5.3.2.1: an entry is not written over by one under
        // another UID.
        let another = calendar
            .entries
            .get(&name)
            .is_some_and(|kept| uid_it_goes_by(&kept.ics) != uid_it_goes_by(&text));
        if another && !any_uid {
            return Reply::status(403).header("Content-Type", XML).body(format!(
                "<d:error {NAMESPACES}><c:no-uid-conflict><d:href>{}</d:href>\
                 </c:no-uid-conflict></d:error>",
                escaped(path)
            ));
        }
        let was = calendar.entries.insert(
            name.clone(),
            Kept {
                etag: next,
                ics: text.clone(),
            },
        );
        calendar.changes.push((next, name));
        self.token = next;
        let old = was.as_ref().map(|kept| kept.ics.as_str());
        if another {
            // To a server that takes it, another UID is another event: the
            // one it held is removed (RFC 6638, 3.2.1.3 and 3.2.2.4) and the
            // one it holds now is new (3.2.1.1).
            self.mailed
                .extend(sent(account.as_deref(), old, None, true));
            self.mailed
                .extend(sent(account.as_deref(), None, Some(&text), true));
        } else {
            self.mailed
                .extend(sent(account.as_deref(), old, Some(&text), true));
        }
        Reply::status(if was.is_none() { 201 } else { 204 }).header("ETag", &etag(next))
    }

    fn delete(&mut self, path: &str, request: &Request) -> Reply {
        let Some((slug, name)) = self.place(path).filter(|(_, entry)| !entry.is_empty()) else {
            return Reply::status(404);
        };
        let next = self.token.saturating_add(1);
        let account = self.address();
        let Some(calendar) = self.calendars.get_mut(&slug) else {
            return Reply::status(404);
        };
        if calendar.read_only {
            return Reply::status(403);
        }
        if !calendar.entries.contains_key(&name) {
            return Reply::status(404);
        }
        if !Self::allowed(request, calendar.entries.get(&name)) {
            return Reply::status(412);
        }
        let was = calendar.entries.remove(&name);
        calendar.changes.push((next, name));
        self.token = next;
        let old = was.as_ref().map(|kept| kept.ics.as_str());
        let reply = request.header("Schedule-Reply").map(str::trim) != Some("F");
        self.mailed
            .extend(sent(account.as_deref(), old, None, reply));
        Reply::status(204)
    }

    fn get(&self, path: &str) -> Reply {
        self.place(path)
            .and_then(|(slug, name)| self.calendars.get(&slug)?.entries.get(&name))
            .map_or_else(
                || Reply::status(404),
                |kept| {
                    Reply::status(200)
                        .header("Content-Type", "text/calendar; charset=utf-8")
                        .header("ETag", &etag(kept.etag))
                        .body(kept.ics.clone())
                },
            )
    }

    fn answer(&mut self, request: &Request) -> Reply {
        if !self.signed_in(request) {
            return Reply::status(401).header("WWW-Authenticate", "Basic realm=\"calendars\"");
        }
        let path = request.target.split(['?', '#']).next().unwrap_or_default();
        match request.method.as_str() {
            "PROPFIND" => self.propfind(path, request.header("Depth").map(str::trim) == Some("1")),
            "REPORT" => self.report(path, &request.body_text()),
            "GET" => self.get(path),
            "PUT" => self.put(path, request),
            "DELETE" => self.delete(path, request),
            _ => Reply::status(405),
        }
    }
}

/// A `CalDAV` server on loopback, for one account.
pub struct Server {
    responder: Mutex<Responder>,
    state: Arc<Mutex<State>>,
}

impl Server {
    fn responder(&self) -> MutexGuard<'_, Responder> {
        self.responder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A server that knows one account, `user` with `password`, and holds no
    /// calendar yet.
    ///
    /// # Panics
    /// If the loopback listener cannot be created.
    #[must_use]
    pub fn start(user: &str, password: &str) -> Self {
        let state = Arc::new(Mutex::new(State::new(user, password)));
        let served = Arc::clone(&state);
        Self {
            responder: Mutex::new(Responder::handle(move |request| {
                Some(lock(&served).answer(request))
            })),
            state,
        }
    }

    /// The address a person types for the account: `http://127.0.0.1:<port>/dav/`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{}/dav/", self.responder().url())
    }

    /// The address of the calendar `slug`, as discovery answers it.
    #[must_use]
    pub fn calendar_url(&self, slug: &str) -> String {
        format!(
            "{}{}{slug}/",
            self.responder().url(),
            lock(&self.state).home()
        )
    }

    /// Give the account a calendar called `name` at `slug`, and answer its
    /// address.
    pub fn add_calendar(&self, slug: &str, name: &str) -> String {
        lock(&self.state).calendars.insert(
            slug.to_owned(),
            Calendar {
                name: name.to_owned(),
                ..Calendar::default()
            },
        );
        self.calendar_url(slug)
    }

    /// Let the account read `slug` and no longer write it.
    pub fn make_read_only(&self, slug: &str) {
        if let Some(calendar) = lock(&self.state).calendars.get_mut(slug) {
            calendar.read_only = true;
        }
    }

    /// Answer `sync-collection` with 501 from now on, as a server that
    /// never learned it does.
    pub fn without_sync_reports(&self) {
        lock(&self.state).syncs = false;
    }

    /// Take an entry written over by one under another `UID` from now on, as
    /// a server does that does not keep RFC 4791's `no-uid-conflict`.
    pub fn takes_another_uid(&self) {
        lock(&self.state).any_uid = true;
    }

    /// Put `ics` into `slug` as the entry `name`, as another client would.
    pub fn put(&self, slug: &str, name: &str, ics: &str) {
        let mut state = lock(&self.state);
        let next = state.token.saturating_add(1);
        if let Some(calendar) = state.calendars.get_mut(slug) {
            calendar.entries.insert(
                name.to_owned(),
                Kept {
                    etag: next,
                    ics: ics.to_owned(),
                },
            );
            calendar.changes.push((next, name.to_owned()));
            state.token = next;
        }
    }

    /// Take the entry `name` out of `slug`, as another client would.
    pub fn remove(&self, slug: &str, name: &str) {
        let mut state = lock(&self.state);
        let next = state.token.saturating_add(1);
        if let Some(calendar) = state.calendars.get_mut(slug)
            && calendar.entries.remove(name).is_some()
        {
            calendar.changes.push((next, name.to_owned()));
            state.token = next;
        }
    }

    /// What `slug` holds now: each entry's name and its text.
    #[must_use]
    pub fn held(&self, slug: &str) -> Vec<(String, String)> {
        lock(&self.state)
            .calendars
            .get(slug)
            .map(|calendar| {
                calendar
                    .entries
                    .iter()
                    .map(|(name, kept)| (name.clone(), kept.ics.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Everyone the server wrote to by itself so far, in order: what it sent
    /// and the address it went to, such as `REQUEST to eve@elsewhere.test`.
    #[must_use]
    pub fn mailed(&self) -> Vec<String> {
        lock(&self.state).mailed.clone()
    }

    /// Every request the server was sent since this was last asked.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.responder().drain_requests()
    }
}

#[cfg(test)]
mod tests {
    use test_http::Request;

    use super::{Calendar, Server, State, base64, lock};

    const ADA: &str = "ada@example.test";
    const BO: &str = "bo@example.test";
    /// A sign-in made up for these tests.
    const PASSWORD: &str = "made-up-for-these-tests";
    const ENTRY: &str = "/dav/calendars/ada/work/a.ics";

    /// How a line reads between `ATTENDEE` and the address it names: left to
    /// the server by saying nothing, or marked.
    const SERVER: &str = "";
    const CLIENT: &str = ";SCHEDULE-AGENT=CLIENT";
    const NONE: &str = ";SCHEDULE-AGENT=NONE";
    /// Whom the server wrote to, where that is nobody.
    const NOBODY: [&str; 0] = [];

    #[test]
    fn credentials_are_written_as_basic_ones() {
        assert_eq!(base64(b"ada:secret"), "YWRhOnNlY3JldA==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b""), "");
    }

    /// Ada's server: she signs in with her address and keeps one calendar.
    fn server() -> State {
        let mut state = State::new(ADA, PASSWORD);
        state.calendars.insert(
            "work".to_owned(),
            Calendar {
                name: "Work".to_owned(),
                ..Calendar::default()
            },
        );
        state
    }

    /// Ada's client, signed in, asks `method` of her one entry.
    fn asked(state: &mut State, method: &str, headers: &[(&str, &str)], body: &str) {
        let sign_in = format!("Basic {}", base64(format!("{ADA}:{PASSWORD}").as_bytes()));
        let mut all = vec![("Authorization".to_owned(), sign_in)];
        all.extend(
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        state.answer(&Request {
            method: method.to_owned(),
            target: ENTRY.to_owned(),
            headers: all,
            body: body.as_bytes().to_vec(),
        });
    }

    fn holds(state: &State) -> bool {
        state
            .calendars
            .get("work")
            .is_some_and(|calendar| calendar.entries.contains_key("a.ics"))
    }

    /// An entry with the lines `people`.
    fn entry(people: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\n\
             DTSTART:20261119T190000Z\r\n{people}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// The entry Ada organises and comes to herself, with Bo's line reading
    /// `line` (`None`: it does not name him).
    fn inviting(line: Option<&str>) -> String {
        let bo = line
            .map(|mark| format!("ATTENDEE{mark}:mailto:{BO}\r\n"))
            .unwrap_or_default();
        entry(&format!(
            "ORGANIZER:mailto:{ADA}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{ADA}\r\n{bo}"
        ))
    }

    /// What the server sent when Ada's client stored her entry where nothing
    /// was, with Bo's line reading `line`.
    fn created(line: &str) -> Vec<String> {
        let mut state = server();
        asked(
            &mut state,
            "PUT",
            &[("If-None-Match", "*")],
            &inviting(Some(line)),
        );
        assert!(holds(&state), "the server took the entry");
        state.mailed
    }

    /// What the server sent when her client stored her entry again with Bo's
    /// line going from `original` to `modified`.
    fn changed(original: Option<&str>, modified: Option<&str>) -> Vec<String> {
        let mut state = server();
        asked(&mut state, "PUT", &[], &inviting(original));
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &inviting(modified));
        assert!(holds(&state), "the server took the entry");
        state.mailed
    }

    /// What the server sent when her client removed her entry, in which Bo's
    /// line read `line`.
    fn removed(line: &str) -> Vec<String> {
        let mut state = server();
        asked(&mut state, "PUT", &[], &inviting(Some(line)));
        state.mailed.clear();
        asked(&mut state, "DELETE", &[], "");
        assert!(!holds(&state), "the server let the entry go");
        state.mailed
    }

    fn request() -> Vec<String> {
        vec![format!("REQUEST to {BO}")]
    }

    fn cancel() -> Vec<String> {
        vec![format!("CANCEL to {BO}")]
    }

    /// RFC 6638, 3.2.1.1 (a new entry), row SERVER, which a line that says
    /// nothing is too: REQUEST. Ada, who organises and is invited herself,
    /// is sent nothing in any row (3.2.1).
    #[test]
    fn create_row_server_is_sent_a_request() {
        assert_eq!(created(SERVER), request());
        assert_eq!(created(";SCHEDULE-AGENT=SERVER"), request());
    }

    /// RFC 6638, 3.2.1.1, row CLIENT: nothing.
    #[test]
    fn create_row_client_is_sent_nothing() {
        assert_eq!(created(CLIENT), NOBODY);
    }

    /// RFC 6638, 3.2.1.1, row NONE: nothing.
    #[test]
    fn create_row_none_is_sent_nothing() {
        assert_eq!(created(NONE), NOBODY);
    }

    /// RFC 6638, 3.2.1.2 (a changed entry), row `<Absent>`, by its columns
    /// `<Removed>`, SERVER, CLIENT and NONE: nothing, REQUEST, nothing,
    /// nothing.
    #[test]
    fn modify_row_absent_is_sent_a_request_only_by_a_line_left_to_the_server() {
        assert_eq!(changed(None, None), NOBODY);
        assert_eq!(changed(None, Some(SERVER)), request());
        assert_eq!(changed(None, Some(CLIENT)), NOBODY);
        assert_eq!(changed(None, Some(NONE)), NOBODY);
    }

    /// RFC 6638, 3.2.1.2, row SERVER, by its columns: CANCEL, REQUEST,
    /// CANCEL, CANCEL. No column of this row sends nothing.
    #[test]
    fn modify_row_server_is_sent_a_cancel_in_every_column_but_its_own() {
        assert_eq!(changed(Some(SERVER), None), cancel());
        assert_eq!(changed(Some(SERVER), Some(SERVER)), request());
        assert_eq!(changed(Some(SERVER), Some(CLIENT)), cancel());
        assert_eq!(changed(Some(SERVER), Some(NONE)), cancel());
    }

    /// RFC 6638, 3.2.1.2, row CLIENT, by its columns: nothing, REQUEST,
    /// nothing, nothing.
    #[test]
    fn modify_row_client_is_sent_a_request_only_by_a_line_left_to_the_server() {
        assert_eq!(changed(Some(CLIENT), None), NOBODY);
        assert_eq!(changed(Some(CLIENT), Some(SERVER)), request());
        assert_eq!(changed(Some(CLIENT), Some(CLIENT)), NOBODY);
        assert_eq!(changed(Some(CLIENT), Some(NONE)), NOBODY);
    }

    /// RFC 6638, 3.2.1.2, row NONE, by its columns: nothing, REQUEST,
    /// nothing, nothing.
    #[test]
    fn modify_row_none_is_sent_a_request_only_by_a_line_left_to_the_server() {
        assert_eq!(changed(Some(NONE), None), NOBODY);
        assert_eq!(changed(Some(NONE), Some(SERVER)), request());
        assert_eq!(changed(Some(NONE), Some(CLIENT)), NOBODY);
        assert_eq!(changed(Some(NONE), Some(NONE)), NOBODY);
    }

    /// RFC 6638, 3.2.1.3 (a removed entry), row SERVER: CANCEL.
    #[test]
    fn remove_row_server_is_sent_a_cancel() {
        assert_eq!(removed(SERVER), cancel());
        assert_eq!(removed(";SCHEDULE-AGENT=SERVER"), cancel());
    }

    /// RFC 6638, 3.2.1.3, row CLIENT: nothing.
    #[test]
    fn remove_row_client_is_sent_nothing() {
        assert_eq!(removed(CLIENT), NOBODY);
    }

    /// RFC 6638, 3.2.1.3, row NONE: nothing.
    #[test]
    fn remove_row_none_is_sent_nothing() {
        assert_eq!(removed(NONE), NOBODY);
    }

    /// RFC 6638, 3.2.3.1, the rows of a PUT over an entry of another kind:
    /// one that scheduled nothing and now does is created (3.2.1.1), and one
    /// that did and no longer does is removed (3.2.1.3).
    #[test]
    fn a_put_that_starts_or_ends_what_an_entry_schedules_is_a_create_or_a_remove() {
        let plain = entry("");
        let mut state = server();
        asked(&mut state, "PUT", &[], &plain);
        assert_eq!(state.mailed, NOBODY, "an entry that names nobody");
        asked(&mut state, "PUT", &[], &inviting(Some(SERVER)));
        assert_eq!(state.mailed, request(), "it starts to schedule");
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &plain);
        assert_eq!(state.mailed, cancel(), "it no longer schedules");

        let mut state = server();
        asked(&mut state, "PUT", &[], &plain);
        asked(&mut state, "PUT", &[], &inviting(Some(CLIENT)));
        asked(&mut state, "PUT", &[], &plain);
        assert_eq!(state.mailed, NOBODY, "the same with the line marked");
    }

    /// RFC 6638, 7.1: a value of `SCHEDULE-AGENT` the server does not know
    /// reads as NONE.
    #[test]
    fn a_mark_the_server_does_not_know_reads_as_none() {
        let other = ";SCHEDULE-AGENT=X-OTHER";
        assert_eq!(created(other), NOBODY);
        assert_eq!(changed(Some(SERVER), Some(other)), cancel());
        assert_eq!(changed(Some(other), Some(SERVER)), request());
        assert_eq!(removed(other), NOBODY);
    }

    /// RFC 6638, 3.2.2.2 to 3.2.2.4 and 8.1, where Bo organises and Ada is
    /// invited: storing the invitation answers nobody; her changed answer,
    /// an occurrence she leaves out and her removal of the entry are
    /// answered to Bo; a removal that says `Schedule-Reply: F` is not, and
    /// nothing is where the organiser's line is marked.
    #[test]
    fn the_organiser_is_answered_for_somebody_invited_unless_the_entry_or_the_removal_says_no() {
        let invited = |organizer: &str, coming: &str, more: &str| {
            entry(&format!(
                "ORGANIZER{organizer}:mailto:{BO}\r\n\
                 ATTENDEE;PARTSTAT={coming}:mailto:{ADA}\r\n{more}"
            ))
        };
        let reply = vec![format!("REPLY to {BO}")];

        let mut state = server();
        asked(&mut state, "PUT", &[], &invited(SERVER, "NEEDS-ACTION", ""));
        asked(&mut state, "PUT", &[], &invited(SERVER, "NEEDS-ACTION", ""));
        assert_eq!(
            state.mailed, NOBODY,
            "an invitation stored, and stored again"
        );
        asked(&mut state, "PUT", &[], &invited(SERVER, "ACCEPTED", ""));
        assert_eq!(state.mailed, reply, "her answer");
        state.mailed.clear();
        let without = "EXDATE:20261126T190000Z\r\n";
        asked(
            &mut state,
            "PUT",
            &[],
            &invited(SERVER, "ACCEPTED", without),
        );
        assert_eq!(state.mailed, reply, "an occurrence she leaves out");
        state.mailed.clear();
        asked(&mut state, "DELETE", &[], "");
        assert_eq!(state.mailed, reply, "her removal");

        let mut state = server();
        asked(&mut state, "PUT", &[], &invited(SERVER, "NEEDS-ACTION", ""));
        asked(&mut state, "DELETE", &[("Schedule-Reply", "F")], "");
        assert!(!holds(&state), "the server let the entry go");
        assert_eq!(state.mailed, NOBODY, "a removal told not to answer");

        let mut state = server();
        asked(&mut state, "PUT", &[], &invited(CLIENT, "NEEDS-ACTION", ""));
        asked(&mut state, "PUT", &[], &invited(CLIENT, "ACCEPTED", ""));
        asked(&mut state, "DELETE", &[], "");
        assert!(!holds(&state), "the server let the entry go");
        assert_eq!(state.mailed, NOBODY, "an organiser the client answers");
    }

    /// Ada's one entry as the server keeps it.
    fn text_it_keeps(state: &State) -> String {
        state
            .calendars
            .get("work")
            .and_then(|calendar| calendar.entries.get("a.ics"))
            .map(|kept| kept.ics.clone())
            .unwrap_or_default()
    }

    /// Her entry under the `UID` another event goes by.
    fn under_another_uid(text: &str) -> String {
        text.replace("UID:a\r\n", "UID:b\r\n")
    }

    /// RFC 4791, 5.3.2.1 (`no-uid-conflict`): an entry is not written over by
    /// one under another `UID`. The server keeps what it held and sends
    /// nothing; under its own `UID` the entry is taken as before.
    #[test]
    fn an_entry_written_over_under_another_uid_is_refused() {
        let mut state = server();
        asked(&mut state, "PUT", &[], &inviting(Some(SERVER)));
        state.mailed.clear();
        asked(
            &mut state,
            "PUT",
            &[],
            &under_another_uid(&inviting(Some(CLIENT))),
        );
        assert_eq!(
            text_it_keeps(&state),
            inviting(Some(SERVER)),
            "what it held stays"
        );
        assert_eq!(state.mailed, NOBODY, "a refused entry sends nothing");
        asked(&mut state, "PUT", &[], &inviting(Some(SERVER)));
        assert_eq!(state.mailed, request(), "under its own UID it is taken");
    }

    /// A server that does not keep that rule reads an entry under another
    /// `UID` as another event: the one it held is removed (RFC 6638, 3.2.1.3)
    /// and the one it holds now is new (3.2.1.1). Whoever it wrote to is told
    /// the event is off and is invited to it again; a line marked for the
    /// client sends nothing either way.
    #[test]
    fn a_server_that_takes_another_uid_removes_what_it_held_and_creates_what_it_holds() {
        let mut state = server();
        state.any_uid = true;
        asked(&mut state, "PUT", &[], &inviting(Some(SERVER)));
        state.mailed.clear();
        let other = under_another_uid(&inviting(Some(SERVER)));
        asked(&mut state, "PUT", &[], &other);
        assert_eq!(text_it_keeps(&state), other, "it took the entry");
        assert_eq!(
            state.mailed,
            [format!("CANCEL to {BO}"), format!("REQUEST to {BO}")]
        );

        let mut state = server();
        state.any_uid = true;
        asked(&mut state, "PUT", &[], &inviting(Some(CLIENT)));
        asked(
            &mut state,
            "PUT",
            &[],
            &under_another_uid(&inviting(Some(CLIENT))),
        );
        assert_eq!(state.mailed, NOBODY, "the same with the line marked");

        // A server keeps the rule until a test tells it otherwise.
        let told = Server::start(ADA, PASSWORD);
        assert!(!lock(&told.state).any_uid);
        told.takes_another_uid();
        assert!(lock(&told.state).any_uid);
    }

    /// Somebody an entry names by another address than a mail address (RFC
    /// 5545, 3.3.3) is written to like anybody else, by the same tables: a
    /// line left to the server invites them, a line that is gone tells them
    /// the event is off, and a line marked for the client sends nothing.
    #[test]
    fn somebody_named_by_another_address_than_a_mail_address_is_written_to_the_same() {
        let by_other_address = |mark: Option<&str>| {
            let line = mark
                .map(|mark| format!("ATTENDEE{mark};CN=\"Fay: Ops\":urn:uuid:Fay-4\r\n"))
                .unwrap_or_default();
            entry(&format!("ORGANIZER:mailto:{ADA}\r\n{line}"))
        };
        let mut state = server();
        asked(
            &mut state,
            "PUT",
            &[("If-None-Match", "*")],
            &by_other_address(Some(SERVER)),
        );
        assert_eq!(state.mailed, ["REQUEST to urn:uuid:fay-4"]);
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &by_other_address(None));
        assert_eq!(state.mailed, ["CANCEL to urn:uuid:fay-4"]);
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &by_other_address(Some(CLIENT)));
        asked(&mut state, "PUT", &[], &by_other_address(None));
        assert_eq!(state.mailed, NOBODY, "the same with the line marked");
    }

    /// RFC 6638, 3.1: the server schedules an entry where every one of its
    /// parts says who organises it. One occurrence that does not say so
    /// makes the entry one the server does not schedule: stored over one it
    /// scheduled, it is that entry's removal (3.2.3.1, 3.2.1.3), and stored
    /// where nothing was, it invites nobody.
    #[test]
    fn an_entry_is_scheduled_only_where_each_of_its_parts_says_who_organises_it() {
        let people = format!("ORGANIZER:mailto:{ADA}\r\nATTENDEE:mailto:{BO}\r\n");
        let repeating = |moved: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\n\
                 DTSTART:20261116T090000Z\r\nRRULE:FREQ=WEEKLY\r\n{people}END:VEVENT\r\n\
                 BEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20261123T090000Z\r\n\
                 DTSTART:20261123T100000Z\r\n{moved}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let mut state = server();
        asked(&mut state, "PUT", &[], &repeating(&people));
        assert_eq!(state.mailed, request(), "each part says who organises it");
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &repeating(&people));
        assert_eq!(state.mailed, request(), "stored again, it goes on");
        state.mailed.clear();
        asked(&mut state, "PUT", &[], &repeating(""));
        assert_eq!(state.mailed, cancel(), "one part no longer says so");

        let mut state = server();
        asked(&mut state, "PUT", &[], &repeating(""));
        assert_eq!(state.mailed, NOBODY, "stored where nothing was");
    }
}
