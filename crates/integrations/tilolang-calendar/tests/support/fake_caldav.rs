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
//! An entry's version is written the same in a listing and in a read (RFC
//! 4791, 5.3.4). A server may be told to write it another way in one of the
//! two ([`Server::lists_versions_weak`], [`Server::lists_versions_bare`],
//! [`Server::reads_versions_weak`]). A write is taken only over the strong
//! tag of what the server holds, whatever it was told (RFC 7232, 3.1).
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
    /// How a listing writes an entry's version.
    listed: Written,
    /// How a read writes an entry's version.
    read: Written,
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

/// How a server writes an entry's version in a listing or in a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    /// As a strong entity-tag, `"3"`: what a write's answer says too.
    Strong,
    /// Marked weak, `W/"3"` (RFC 7232, 2.3).
    Weak,
    /// Without its quotes, `3`: no entity-tag by the grammar.
    Bare,
}

fn etag_as(version: u64, written: Written) -> String {
    match written {
        Written::Strong => etag(version),
        Written::Weak => format!("W/{}", etag(version)),
        Written::Bare => version.to_string(),
    }
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
/// request said not to answer (`reply`, section 8.1). The entry has gone too
/// where it is stored again as one the server no longer schedules (3.2.3.1):
/// it no longer says who organises it, or no longer invites the person.
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
    // RFC 6638, 3.1: it is an invitation the server schedules while it says
    // who organises it and invites the person.
    let still_invited = now.organizer.is_some() && own(&now).is_some();
    let answers = match new {
        None => reply,
        // 3.2.3.1: stored as an entry the server no longer schedules, the
        // invitation was removed (3.2.2.4). A PUT cannot say that nobody is
        // to be answered: 8.1 is for a DELETE.
        Some(_) if !still_invited => true,
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
            listed: Written::Strong,
            read: Written::Strong,
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
        let Some((scheme, sent)) = request
            .header("Authorization")
            .and_then(|sent| sent.split_once(' '))
        else {
            return false;
        };
        let sent = sent.trim();
        let wanted = base64(format!("{}:{}", self.user, self.password).as_bytes());
        scheme.eq_ignore_ascii_case("Basic") && sent == wanted
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

    fn entry_response(href: &str, kept: &Kept, listed: Written) -> String {
        found(
            href,
            &format!(
                "<d:getetag>{}</d:getetag><c:calendar-data>{}</c:calendar-data>",
                escaped(&etag_as(kept.etag, listed)),
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
                .map(|(name, kept)| {
                    Self::entry_response(&format!("{collection}{name}"), kept, self.listed)
                })
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
                calendar.entries.get(name).map_or_else(
                    || gone(&href),
                    |kept| Self::entry_response(&href, kept, self.listed),
                )
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
                        .header("ETag", &etag_as(kept.etag, self.read))
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
        Self::serving(State::new(user, password))
    }

    fn serving(state: State) -> Self {
        let state = Arc::new(Mutex::new(state));
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
    #[allow(
        dead_code,
        reason = "only the calendar app's tests, which share this server, ask it this"
    )]
    pub fn takes_another_uid(&self) {
        lock(&self.state).any_uid = true;
    }

    /// Write an entry's version in a listing as a weak tag from now on:
    /// `W/"3"` where a read says `"3"`.
    pub fn lists_versions_weak(&self) {
        lock(&self.state).listed = Written::Weak;
    }

    /// Write an entry's version in a listing without its quotes from now
    /// on: `3` where a read says `"3"`.
    pub fn lists_versions_bare(&self) {
        lock(&self.state).listed = Written::Bare;
    }

    /// Answer a read with the entry's version as a weak tag from now on, as
    /// a proxy does that changes what it passes on (RFC 7232, 2.1).
    pub fn reads_versions_weak(&self) {
        lock(&self.state).read = Written::Weak;
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
