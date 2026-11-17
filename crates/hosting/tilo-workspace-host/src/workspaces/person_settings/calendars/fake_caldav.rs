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

impl State {
    fn principal(&self) -> String {
        format!("/dav/principals/{}/", self.user)
    }

    fn home(&self) -> String {
        format!("/dav/calendars/{}/", self.user)
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
        let Some(calendar) = self.calendars.get_mut(&slug) else {
            return Reply::status(404);
        };
        if calendar.read_only {
            return Reply::status(403);
        }
        if !Self::allowed(request, calendar.entries.get(&name)) {
            return Reply::status(412);
        }
        let created = calendar
            .entries
            .insert(
                name.clone(),
                Kept {
                    etag: next,
                    ics: request.body_text(),
                },
            )
            .is_none();
        calendar.changes.push((next, name));
        self.token = next;
        Reply::status(if created { 201 } else { 204 }).header("ETag", &etag(next))
    }

    fn delete(&mut self, path: &str, request: &Request) -> Reply {
        let Some((slug, name)) = self.place(path).filter(|(_, entry)| !entry.is_empty()) else {
            return Reply::status(404);
        };
        let next = self.token.saturating_add(1);
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
        calendar.entries.remove(&name);
        calendar.changes.push((next, name));
        self.token = next;
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
        let state = Arc::new(Mutex::new(State {
            user: user.to_owned(),
            password: password.to_owned(),
            calendars: BTreeMap::new(),
            token: 1,
            syncs: true,
        }));
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

    /// Every request the server was sent since this was last asked.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.responder().drain_requests()
    }
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn credentials_are_written_as_basic_ones() {
        assert_eq!(base64(b"ada:secret"), "YWRhOnNlY3JldA==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b""), "");
    }
}
