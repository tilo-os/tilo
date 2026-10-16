//! An in-memory Dropbox API v2 on the shared test responder.
//!
//! Three listeners stand in for the three hosts (`api`, `content`,
//! `notify`) and each answers only its own routes, so a call sent to the
//! wrong host fails as it would against Dropbox. The store is
//! case-insensitive and path-addressed with stable ids, like Dropbox:
//!
//! - `list_folder` / `continue` / `get_latest_cursor` with paging (`limit`),
//!   coalesced deltas, `deleted` entries that carry only a path (a deleted
//!   folder reports only itself; a move reports the old path deleted and
//!   every moved item at its new path), and cursors that expire with
//!   `reset`;
//! - `list_folder/longpoll` that holds the request until a change lands in
//!   the cursor's scope or the timeout (30–480 s, scaled by
//!   [`FakeDropbox::set_longpoll_unit`]) passes, refuses an
//!   `Authorization` header, and can ask for a `backoff`;
//! - uploads with `WriteMode` add/overwrite/update(rev),
//!   `strict_conflict`, `content_hash` checks and parent auto-creation;
//!   upload sessions with offsets, `incorrect_offset` + `correct_offset`,
//!   closed and unknown sessions;
//! - ranged downloads with `Dropbox-API-Result`, downloads by `rev:`,
//!   revision history (surviving deletion), restore, shared links, space
//!   usage, the current account's namespaces and `Dropbox-API-Path-Root`
//!   validation.
//!
//! Faults ([`FakeFault`]) apply to the next request they fit, and the
//! server can go offline (connections are closed unanswered).
//!
//! Test support only (feature `fake-server`).

#![allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    reason = "a test fixture: counters are tiny and a poisoned lock has already failed the test"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use test_http::{Reply, Request, Responder};

use crate::content_hash::content_hash;
use crate::{DropboxConfig, Endpoints};

/// The access token the fake accepts.
pub const TOKEN: &str = "fake-dropbox-access-token";
/// The account's own namespace.
pub const HOME_NAMESPACE: &str = "2000";
/// The team space's root namespace ([`FakeDropbox::team`] accounts).
pub const TEAM_NAMESPACE: &str = "1000";

const MAX_UPLOAD: usize = 150 * 1024 * 1024;

/// A fault for the next request it fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFault {
    /// Any request: 429 `too_many_requests` with `Retry-After: 1`.
    RateLimited,
    /// Any request: 500.
    ServerError,
    /// Any authenticated request: 401 `expired_access_token`, once.
    ExpiredCredential,
    /// Any request: it takes effect, but the connection closes before the
    /// response, as when a network drops mid-call.
    DropResponse,
    /// The next download declares its full length but sends half the body.
    TruncatedBody,
    /// The next download sends half the body, declares no length and closes
    /// the connection.
    ClosedEarly,
    /// The next download flips one byte of the body.
    CorruptBody,
    /// The next long-poll answer asks the client to back off this many
    /// seconds.
    Backoff(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    Api,
    Content,
    Notify,
}

#[derive(Clone, Debug)]
struct Version {
    rev: String,
    bytes: Arc<Vec<u8>>,
    modified: SystemTime,
    hash: String,
}

#[derive(Clone, Debug)]
struct Node {
    id: String,
    display: String,
    folder: bool,
    current: Option<Version>,
}

#[derive(Clone, Debug)]
struct Tombstone {
    display: String,
    id: String,
    folder: bool,
}

#[derive(Debug)]
struct Session {
    bytes: Vec<u8>,
    closed: bool,
}

struct State {
    nodes: BTreeMap<String, Node>,
    /// Every version of every file id, oldest first; survives deletion.
    history: BTreeMap<String, Vec<Version>>,
    tombstones: BTreeMap<String, Tombstone>,
    seq: u64,
    ids: u64,
    log: Vec<(u64, String)>,
    epoch: u64,
    sessions: BTreeMap<String, Session>,
    links: BTreeMap<String, String>,
    faults: Vec<FakeFault>,
    offline: bool,
    team: bool,
    longpoll_unit: Duration,
    quota: u64,
    counts: BTreeMap<String, usize>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// A running fake Dropbox. Dropping it stops the server.
pub struct FakeDropbox {
    api: Responder,
    content: Responder,
    notify: Responder,
    shared: Shared,
}

fn lower(path: &str) -> String {
    path.to_lowercase()
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

fn within(path: &str, scope: &str) -> bool {
    scope.is_empty()
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn is_rev(rev: &str) -> bool {
    rev.len() >= 9 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

fn json_reply(status: u16, value: &Value) -> Reply {
    Reply::status(status)
        .header("Content-Type", "application/json")
        .body(value.to_string())
}

fn ok(value: &Value) -> Reply {
    json_reply(200, value)
}

fn route_error(summary: &str, error: Value) -> Reply {
    json_reply(
        409,
        &json!({"error_summary": format!("{summary}/..."), "error": error}),
    )
}

fn bad_input(message: &str) -> Reply {
    Reply::status(400)
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(format!("Error in call to API function: {message}"))
}

fn not_found(outer: &str) -> Reply {
    route_error(
        &format!("{outer}/not_found"),
        json!({".tag": outer, outer: {".tag": "not_found"}}),
    )
}

fn write_conflict(outer: &str, what: &str) -> Reply {
    route_error(
        &format!("{outer}/conflict/{what}"),
        json!({".tag": outer, outer: {".tag": "conflict", "conflict": {".tag": what}}}),
    )
}

fn upload_conflict(what: &str) -> Reply {
    route_error(
        &format!("path/conflict/{what}"),
        json!({".tag": "path", "path": {
            "reason": {".tag": "conflict", "conflict": {".tag": what}},
            "upload_session_id": ""
        }}),
    )
}

fn hex_json(value: &Value) -> String {
    hex::encode(value.to_string())
}

fn unhex_json(text: &str) -> Option<Value> {
    serde_json::from_slice(&hex::decode(text).ok()?).ok()
}

/// A decoded list cursor.
#[derive(Clone, Debug)]
struct ListCursor {
    epoch: u64,
    scope: String,
    recursive: bool,
    seq: u64,
    /// Position in an initial enumeration that is still paging.
    skip: Option<usize>,
}

impl ListCursor {
    fn encode(&self) -> String {
        format!(
            "AAF{}",
            hex_json(&json!({
                "e": self.epoch, "p": self.scope, "r": self.recursive,
                "s": self.seq, "k": self.skip,
            }))
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let value = unhex_json(text.strip_prefix("AAF")?)?;
        Some(Self {
            epoch: value.get("e")?.as_u64()?,
            scope: value.get("p")?.as_str()?.to_owned(),
            recursive: value.get("r")?.as_bool()?,
            seq: value.get("s")?.as_u64()?,
            skip: value
                .get("k")
                .and_then(Value::as_u64)
                .and_then(|k| usize::try_from(k).ok()),
        })
    }

    fn in_scope(&self, path: &str) -> bool {
        if self.recursive {
            within(path, &self.scope)
        } else {
            parent(path) == self.scope
        }
    }
}

impl State {
    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn touch(&mut self, key: &str) {
        let seq = self.bump();
        self.log.push((seq, key.to_owned()));
    }

    fn new_id(&mut self) -> String {
        self.ids += 1;
        format!("id:fakeAAAAAAAAAAAA{:06}", self.ids)
    }

    fn new_version(&mut self, bytes: Vec<u8>) -> Version {
        let seq = self.bump();
        Version {
            rev: format!("{seq:015x}"),
            hash: content_hash(&bytes),
            bytes: Arc::new(bytes),
            modified: SystemTime::now(),
        }
    }

    fn key_of_id(&self, id: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|(_, node)| node.id == id)
            .map(|(key, _)| key.clone())
    }

    /// Resolve an id or `/path` argument to a store key. `Ok(None)`: no
    /// such item. `Err`: the argument is malformed (400).
    fn resolve(&self, path: &str) -> Result<Option<String>, Reply> {
        if path.starts_with("id:") {
            return Ok(self.key_of_id(path));
        }
        if path.starts_with('/') && !path.ends_with('/') {
            let key = lower(path);
            return Ok(self.nodes.contains_key(&key).then_some(key));
        }
        Err(bad_input("path does not match pattern"))
    }

    fn metadata(&self, key: &str, tagged: bool) -> Value {
        let Some(node) = self.nodes.get(key) else {
            return Value::Null;
        };
        let mut value = json!({
            "name": name_of(&node.display),
            "id": node.id,
            "path_lower": key,
            "path_display": node.display,
        });
        if let (Some(version), Some(object)) = (&node.current, value.as_object_mut()) {
            let modified = crate::time::format(version.modified);
            object.insert("rev".into(), json!(version.rev));
            object.insert("size".into(), json!(version.bytes.len()));
            object.insert("server_modified".into(), json!(modified));
            object.insert("client_modified".into(), json!(modified));
            object.insert("content_hash".into(), json!(version.hash));
            object.insert("is_downloadable".into(), json!(true));
        }
        if tagged && let Some(object) = value.as_object_mut() {
            let tag = if node.folder { "folder" } else { "file" };
            object.insert(".tag".into(), json!(tag));
        }
        value
    }

    fn version_metadata(id: &str, display: &str, version: &Version) -> Value {
        let modified = crate::time::format(version.modified);
        json!({
            "name": name_of(display),
            "id": id,
            "path_lower": lower(display),
            "path_display": display,
            "rev": version.rev,
            "size": version.bytes.len(),
            "server_modified": modified,
            "client_modified": modified,
            "content_hash": version.hash,
            "is_downloadable": true,
        })
    }

    fn deleted_metadata(key: &str, display: &str) -> Value {
        json!({".tag": "deleted", "name": name_of(display), "path_lower": key, "path_display": display})
    }

    /// Create missing ancestor folders of `display`, as Dropbox does, and
    /// return the path as Dropbox would display a new item there: existing
    /// ancestors keep their own case, new components take the caller's.
    /// (An existing item keeps its display path when written.)
    fn ensure_parents(&mut self, display: &str, outer: &str) -> Result<String, Reply> {
        let mut prefix = String::new();
        let parts: Vec<&str> = display.split('/').filter(|p| !p.is_empty()).collect();
        let (last, ancestors) = parts.split_last().ok_or_else(|| bad_input("empty path"))?;
        for part in ancestors {
            let candidate = format!("{prefix}/{part}");
            prefix = match self.nodes.get(&lower(&candidate)) {
                Some(node) if node.folder => node.display.clone(),
                Some(_) => return Err(write_conflict(outer, "file_ancestor")),
                None => {
                    self.insert_folder(&candidate);
                    candidate
                }
            };
        }
        Ok(format!("{prefix}/{last}"))
    }

    fn insert_folder(&mut self, display: &str) -> String {
        let key = lower(display);
        let id = self.new_id();
        self.nodes.insert(
            key.clone(),
            Node {
                id,
                display: display.to_owned(),
                folder: true,
                current: None,
            },
        );
        self.tombstones.remove(&key);
        self.touch(&key);
        key
    }

    /// Write `bytes` to the file at `display` (existing or new).
    fn write_file(&mut self, display: &str, bytes: Vec<u8>) -> String {
        let key = lower(display);
        let version = self.new_version(bytes);
        let existing = self.nodes.get_mut(&key).map(|node| {
            node.current = Some(version.clone());
            node.id.clone()
        });
        let id = if let Some(id) = existing {
            id
        } else {
            let id = self.new_id();
            self.nodes.insert(
                key.clone(),
                Node {
                    id: id.clone(),
                    display: display.to_owned(),
                    folder: false,
                    current: Some(version.clone()),
                },
            );
            id
        };
        self.history.entry(id).or_default().push(version);
        self.tombstones.remove(&key);
        self.touch(&key);
        key
    }

    fn remove_tree(&mut self, key: &str) {
        let doomed: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| within(k, key))
            .cloned()
            .collect();
        for doomed_key in &doomed {
            if let Some(node) = self.nodes.remove(doomed_key) {
                self.tombstones.insert(
                    doomed_key.clone(),
                    Tombstone {
                        display: node.display,
                        id: node.id,
                        folder: node.folder,
                    },
                );
            }
        }
        // Dropbox reports a deleted folder, not its contents.
        self.touch(key);
    }

    fn move_tree(&mut self, from: &str, to_display: &str) -> String {
        let to_key = lower(to_display);
        let moving: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| within(k, from))
            .cloned()
            .collect();
        let from_display = self
            .nodes
            .get(from)
            .map_or_else(|| from.to_owned(), |n| n.display.clone());
        let mut moved = Vec::new();
        for key in moving {
            if let Some(mut node) = self.nodes.remove(&key) {
                let rest_display = node
                    .display
                    .get(from.len()..)
                    .unwrap_or_default()
                    .to_owned();
                node.display = format!("{to_display}{rest_display}");
                let new_key = lower(&node.display);
                self.tombstones.insert(
                    key.clone(),
                    Tombstone {
                        display: format!("{from_display}{rest_display}"),
                        id: node.id.clone(),
                        folder: node.folder,
                    },
                );
                self.tombstones.remove(&new_key);
                self.nodes.insert(new_key.clone(), node);
                moved.push(new_key);
            }
        }
        self.touch(from);
        for key in moved {
            self.touch(&key);
        }
        to_key
    }

    fn listing(&self, cursor: &ListCursor) -> Vec<Value> {
        let mut entries = Vec::new();
        if cursor.recursive && !cursor.scope.is_empty() {
            entries.push(self.metadata(&cursor.scope, true));
        }
        for key in self.nodes.keys() {
            if key != &cursor.scope && cursor.in_scope(key) {
                entries.push(self.metadata(key, true));
            }
        }
        entries
    }

    fn page(&self, cursor: &ListCursor, limit: usize) -> Value {
        if let Some(skip) = cursor.skip {
            let all = self.listing(cursor);
            let end = skip.saturating_add(limit).min(all.len());
            let entries = all.get(skip.min(end)..end).unwrap_or_default().to_vec();
            let has_more = end < all.len();
            let next = ListCursor {
                skip: has_more.then_some(end),
                ..cursor.clone()
            };
            return json!({"entries": entries, "cursor": next.encode(), "has_more": has_more});
        }
        let mut last: BTreeMap<&str, u64> = BTreeMap::new();
        for (seq, key) in &self.log {
            if *seq > cursor.seq && cursor.in_scope(key) {
                last.insert(key.as_str(), *seq);
            }
        }
        let mut changed: Vec<(u64, &str)> = last.into_iter().map(|(k, s)| (s, k)).collect();
        changed.sort_unstable();
        let has_more = changed.len() > limit;
        let taken = changed.get(..changed.len().min(limit)).unwrap_or_default();
        let entries: Vec<Value> = taken
            .iter()
            .map(|(_, key)| {
                if self.nodes.contains_key(*key) {
                    self.metadata(key, true)
                } else {
                    let display = self
                        .tombstones
                        .get(*key)
                        .map_or_else(|| (*key).to_owned(), |t| t.display.clone());
                    Self::deleted_metadata(key, &display)
                }
            })
            .collect();
        let seq = if has_more {
            taken.last().map_or(cursor.seq, |(s, _)| *s)
        } else {
            self.seq
        };
        let next = ListCursor {
            seq,
            ..cursor.clone()
        };
        json!({"entries": entries, "cursor": next.encode(), "has_more": has_more})
    }

    fn has_changes(&self, cursor: &ListCursor) -> bool {
        cursor.skip.is_some()
            || self
                .log
                .iter()
                .any(|(seq, key)| *seq > cursor.seq && cursor.in_scope(key))
    }

    fn take_fault(&mut self, host: Host, route: &str, authenticated: bool) -> Option<FakeFault> {
        let position = self.faults.iter().position(|fault| match fault {
            FakeFault::RateLimited | FakeFault::ServerError | FakeFault::DropResponse => true,
            FakeFault::ExpiredCredential => authenticated,
            FakeFault::TruncatedBody | FakeFault::ClosedEarly | FakeFault::CorruptBody => {
                route == "files/download"
            }
            FakeFault::Backoff(_) => host == Host::Notify,
        })?;
        Some(self.faults.remove(position))
    }

    fn find_version(&self, rev: &str) -> Option<(String, Version)> {
        self.history.iter().find_map(|(id, versions)| {
            versions
                .iter()
                .find(|v| v.rev == rev)
                .map(|v| (id.clone(), v.clone()))
        })
    }

    fn display_of_id(&self, id: &str) -> Option<String> {
        self.nodes
            .values()
            .find(|n| n.id == id)
            .map(|n| n.display.clone())
            .or_else(|| {
                self.tombstones
                    .values()
                    .find(|t| t.id == id)
                    .map(|t| t.display.clone())
            })
    }
}

impl FakeDropbox {
    /// An individual account.
    pub fn start() -> Self {
        Self::launch(false)
    }

    /// A team member whose team space is the root namespace
    /// ([`TEAM_NAMESPACE`]), distinct from the home namespace.
    pub fn team() -> Self {
        Self::launch(true)
    }

    fn launch(team: bool) -> Self {
        let state = State {
            nodes: BTreeMap::new(),
            history: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            seq: 0,
            ids: 0,
            log: Vec::new(),
            epoch: 1,
            sessions: BTreeMap::new(),
            links: BTreeMap::new(),
            faults: Vec::new(),
            offline: false,
            team,
            longpoll_unit: Duration::from_secs(1),
            quota: 2 * 1024 * 1024 * 1024,
            counts: BTreeMap::new(),
        };
        let shared: Shared = Arc::new((Mutex::new(state), Condvar::new()));
        let responder = |host: Host| {
            let shared = Arc::clone(&shared);
            Responder::handle(move |request| handle(&shared, host, request))
        };
        Self {
            api: responder(Host::Api),
            content: responder(Host::Content),
            notify: responder(Host::Notify),
            shared,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.0.lock().expect("fake state")
    }

    fn changed(&self) {
        self.shared.1.notify_all();
    }

    pub fn endpoints(&self) -> Endpoints {
        Endpoints {
            api: self.api.url(),
            content: self.content.url(),
            notify: self.notify.url(),
        }
    }

    /// A mount of `root` on this server, with 64 KiB upload chunks and
    /// three-entry list pages so tests exercise sessions and paging cheaply.
    pub fn config(&self, root: &str) -> DropboxConfig {
        let mut config = DropboxConfig::new(root);
        config.endpoints = self.endpoints();
        config.upload_chunk = 64 * 1024;
        config.page_limit = Some(3);
        config
    }

    /// Create or replace a file at `path` (`/a/b.txt`), creating parents.
    pub fn put(&self, path: &str, bytes: &[u8]) {
        {
            let mut state = self.lock();
            let display = state
                .ensure_parents(&format!("/{}", path.trim_matches('/')), "path")
                .expect("parents are folders");
            state.write_file(&display, bytes.to_vec());
        }
        self.changed();
    }

    /// Create a folder and its parents.
    pub fn mkdir(&self, path: &str) {
        {
            let mut state = self.lock();
            let display = state
                .ensure_parents(&format!("/{}", path.trim_matches('/')), "path")
                .expect("parents are folders");
            if !state.nodes.contains_key(&lower(&display)) {
                state.insert_folder(&display);
            }
        }
        self.changed();
    }

    /// Delete the item at `path` and everything below it.
    pub fn remove(&self, path: &str) {
        {
            let mut state = self.lock();
            let key = lower(&format!("/{}", path.trim_matches('/')));
            if state.nodes.contains_key(&key) {
                state.remove_tree(&key);
            }
        }
        self.changed();
    }

    /// A file's current bytes (case-insensitive path).
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        let key = lower(&format!("/{}", path.trim_matches('/')));
        self.lock()
            .nodes
            .get(&key)
            .and_then(|n| n.current.as_ref())
            .map(|v| v.bytes.to_vec())
    }

    /// Every `path_display` in the store, sorted by `path_lower`.
    pub fn paths(&self) -> Vec<String> {
        self.lock()
            .nodes
            .values()
            .map(|n| n.display.clone())
            .collect()
    }

    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    pub fn inject(&self, fault: FakeFault) {
        self.lock().faults.push(fault);
    }

    /// Expire every cursor issued so far (`reset`).
    pub fn reset_cursors(&self) {
        self.lock().epoch += 1;
        self.changed();
    }

    /// How long one requested long-poll second lasts (1 s by default).
    pub fn set_longpoll_unit(&self, unit: Duration) {
        self.lock().longpoll_unit = unit;
    }

    pub fn set_quota(&self, allocated: u64) {
        self.lock().quota = allocated;
    }

    /// How many requests `route` (e.g. `"files/list_folder"`) received.
    pub fn count(&self, route: &str) -> usize {
        self.lock().counts.get(route).copied().unwrap_or(0)
    }

    /// Open upload sessions.
    pub fn open_sessions(&self) -> usize {
        self.lock().sessions.len()
    }
}

fn host_serves(host: Host, route: &str) -> bool {
    match host {
        Host::Notify => route == "files/list_folder/longpoll",
        Host::Content => matches!(
            route,
            "files/download"
                | "files/upload"
                | "files/upload_session/start"
                | "files/upload_session/append_v2"
                | "files/upload_session/finish"
        ),
        Host::Api => matches!(
            route,
            "files/get_metadata"
                | "files/list_folder"
                | "files/list_folder/continue"
                | "files/list_folder/get_latest_cursor"
                | "files/create_folder_v2"
                | "files/move_v2"
                | "files/delete_v2"
                | "files/list_revisions"
                | "files/restore"
                | "sharing/create_shared_link_with_settings"
                | "sharing/list_shared_links"
                | "users/get_space_usage"
                | "users/get_current_account"
        ),
    }
}

fn check_path_root(state: &State, request: &Request) -> Result<(), Reply> {
    let Some(raw) = request.header("dropbox-api-path-root") else {
        return Ok(());
    };
    let value: Value = serde_json::from_str(raw).map_err(|_| bad_input("Dropbox-API-Path-Root"))?;
    let root = if state.team {
        TEAM_NAMESPACE
    } else {
        HOME_NAMESPACE
    };
    let valid = match value.get(".tag").and_then(Value::as_str) {
        Some("home") => true,
        Some("root") => value.get("root").and_then(Value::as_str) == Some(root),
        Some("namespace_id") => value
            .get("namespace_id")
            .and_then(Value::as_str)
            .is_some_and(|ns| ns == root || ns == HOME_NAMESPACE),
        _ => return Err(bad_input("Dropbox-API-Path-Root")),
    };
    if valid {
        Ok(())
    } else {
        Err(json_reply(
            422,
            &json!({
                "error_summary": "invalid_root/...",
                "error": {".tag": "invalid_root", "invalid_root": {
                    ".tag": if state.team { "team" } else { "user" },
                    "root_namespace_id": root,
                    "home_namespace_id": HOME_NAMESPACE,
                }},
            }),
        ))
    }
}

fn argument(host: Host, request: &Request) -> Result<Value, Reply> {
    match host {
        Host::Content => {
            let raw = request
                .header("dropbox-api-arg")
                .ok_or_else(|| bad_input("missing Dropbox-API-Arg"))?;
            if !raw.is_ascii() {
                return Err(bad_input("Dropbox-API-Arg must be ASCII"));
            }
            serde_json::from_str(raw).map_err(|_| bad_input("Dropbox-API-Arg is not JSON"))
        }
        Host::Api | Host::Notify => {
            let text = request.body_text();
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            let json_type = request
                .header("content-type")
                .is_some_and(|t| t.starts_with("application/json"));
            if !json_type {
                return Err(bad_input("Content-Type must be application/json"));
            }
            serde_json::from_str(&text).map_err(|_| bad_input("the body is not JSON"))
        }
    }
}

fn str_arg<'a>(arg: &'a Value, name: &str) -> Result<&'a str, Reply> {
    arg.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_input(&format!("missing {name}")))
}

fn handle(shared: &Shared, host: Host, request: &Request) -> Option<Reply> {
    let (lock, condvar) = &**shared;
    let mut state = lock.lock().expect("fake state");
    if state.offline {
        return None;
    }
    let target = request.target.split('?').next().unwrap_or_default();
    let Some(route) = target.strip_prefix("/2/") else {
        return Some(Reply::status(404));
    };
    *state.counts.entry(route.to_owned()).or_default() += 1;
    let authenticated = host != Host::Notify;
    let fault = state.take_fault(host, route, authenticated);
    match fault {
        Some(FakeFault::RateLimited) => {
            return Some(
                json_reply(
                    429,
                    &json!({"error_summary": "too_many_requests/...",
                            "error": {"reason": {".tag": "too_many_requests"}, "retry_after": 1}}),
                )
                .header("Retry-After", "1"),
            );
        }
        Some(FakeFault::ServerError) => return Some(Reply::status(500)),
        Some(FakeFault::ExpiredCredential) => {
            return Some(json_reply(
                401,
                &json!({"error_summary": "expired_access_token/...",
                        "error": {".tag": "expired_access_token"}}),
            ));
        }
        _ => {}
    }
    if authenticated {
        if request.header("authorization") != Some(format!("Bearer {TOKEN}").as_str()) {
            return Some(json_reply(
                401,
                &json!({"error_summary": "invalid_access_token/...",
                        "error": {".tag": "invalid_access_token"}}),
            ));
        }
    } else if request.header("authorization").is_some() {
        return Some(bad_input(
            "Your request includes an Authorization header, but this function does not use it.",
        ));
    }
    if !host_serves(host, route) {
        return Some(Reply::status(404).body("Unknown API function"));
    }
    if let Err(reply) = check_path_root(&state, request) {
        return Some(reply);
    }
    if host == Host::Content
        && route != "files/download"
        && request.header("content-type") != Some("application/octet-stream")
    {
        return Some(bad_input("Content-Type must be application/octet-stream"));
    }
    let arg = match argument(host, request) {
        Ok(arg) => arg,
        Err(reply) => return Some(reply),
    };
    let before = state.seq;
    let reply = if route == "files/list_folder/longpoll" {
        let (reply, guard) = longpoll(state, condvar, &arg, fault);
        state = guard;
        reply
    } else {
        let result = route_call(&mut state, route, &arg, request, fault);
        result.unwrap_or_else(|reply| reply)
    };
    let changed = state.seq != before;
    drop(state);
    if changed {
        condvar.notify_all();
    }
    if fault == Some(FakeFault::DropResponse) {
        return None;
    }
    Some(reply)
}

fn longpoll<'a>(
    mut state: MutexGuard<'a, State>,
    condvar: &'a Condvar,
    arg: &Value,
    fault: Option<FakeFault>,
) -> (Reply, MutexGuard<'a, State>) {
    let Ok(raw) = str_arg(arg, "cursor") else {
        return (bad_input("missing cursor"), state);
    };
    let timeout = arg.get("timeout").and_then(Value::as_u64).unwrap_or(30);
    if !(30..=480).contains(&timeout) {
        return (bad_input("timeout must be between 30 and 480"), state);
    }
    let Some(cursor) = ListCursor::decode(raw) else {
        return (bad_input("invalid cursor"), state);
    };
    let deadline = Instant::now()
        + state
            .longpoll_unit
            .saturating_mul(u32::try_from(timeout).unwrap_or(480));
    let changes = loop {
        if cursor.epoch != state.epoch {
            return (route_error("reset", json!({".tag": "reset"})), state);
        }
        if state.has_changes(&cursor) {
            break true;
        }
        let now = Instant::now();
        if now >= deadline {
            break false;
        }
        state = condvar
            .wait_timeout(state, deadline - now)
            .expect("fake state")
            .0;
    };
    let mut body = json!({ "changes": changes });
    if let (Some(FakeFault::Backoff(seconds)), Some(object)) = (fault, body.as_object_mut()) {
        object.insert("backoff".into(), json!(seconds));
    }
    (ok(&body), state)
}

fn list_limit(arg: &Value) -> Result<usize, Reply> {
    match arg.get("limit").and_then(Value::as_u64) {
        None => Ok(2000),
        Some(limit @ 1..=2000) => Ok(usize::try_from(limit).unwrap_or(2000)),
        Some(_) => Err(bad_input("limit must be between 1 and 2000")),
    }
}

fn scope_cursor(state: &State, arg: &Value) -> Result<ListCursor, Reply> {
    let path = str_arg(arg, "path")?;
    let scope = if path.is_empty() {
        String::new()
    } else {
        match state.resolve(path)? {
            Some(key) if state.nodes.get(&key).is_some_and(|n| n.folder) => key,
            Some(_) => {
                return Err(route_error(
                    "path/not_folder",
                    json!({".tag": "path", "path": {".tag": "not_folder"}}),
                ));
            }
            None => return Err(not_found("path")),
        }
    };
    Ok(ListCursor {
        epoch: state.epoch,
        scope,
        recursive: arg
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        seq: state.seq,
        skip: Some(0),
    })
}

fn route_call(
    state: &mut State,
    route: &str,
    arg: &Value,
    request: &Request,
    fault: Option<FakeFault>,
) -> Result<Reply, Reply> {
    match route {
        "files/get_metadata" => {
            let path = str_arg(arg, "path")?;
            if path.is_empty() {
                return Err(bad_input("the root folder is unsupported"));
            }
            if let Some(key) = state.resolve(path)? {
                return Ok(ok(&state.metadata(&key, true)));
            }
            let include_deleted = arg
                .get("include_deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match state.tombstones.get(&lower(path)) {
                Some(tomb) if include_deleted => {
                    Ok(ok(&State::deleted_metadata(&lower(path), &tomb.display)))
                }
                _ => Err(not_found("path")),
            }
        }
        "files/list_folder" => {
            let limit = list_limit(arg)?;
            let cursor = scope_cursor(state, arg)?;
            Ok(ok(&state.page(&cursor, limit)))
        }
        "files/list_folder/get_latest_cursor" => {
            let cursor = ListCursor {
                skip: None,
                ..scope_cursor(state, arg)?
            };
            Ok(ok(&json!({"cursor": cursor.encode()})))
        }
        "files/list_folder/continue" => {
            let cursor = ListCursor::decode(str_arg(arg, "cursor")?)
                .ok_or_else(|| bad_input("invalid cursor"))?;
            if cursor.epoch != state.epoch {
                return Err(route_error("reset", json!({".tag": "reset"})));
            }
            Ok(ok(&state.page(&cursor, 2000.min(list_limit(arg)?))))
        }
        "files/create_folder_v2" => {
            let path = str_arg(arg, "path")?;
            if !path.starts_with('/') || path.ends_with('/') {
                return Err(bad_input("path does not match pattern"));
            }
            if let Some(node) = state.nodes.get(&lower(path)) {
                let what = if node.folder { "folder" } else { "file" };
                return Err(write_conflict("path", what));
            }
            let display = state.ensure_parents(path, "path")?;
            let key = state.insert_folder(&display);
            Ok(ok(&json!({"metadata": state.metadata(&key, false)})))
        }
        "files/delete_v2" => {
            let Some(key) = state.resolve(str_arg(arg, "path")?)? else {
                return Err(not_found("path_lookup"));
            };
            if let Some(parent_rev) = arg.get("parent_rev").and_then(Value::as_str) {
                let node = state.nodes.get(&key);
                let current = node
                    .and_then(|n| n.current.as_ref())
                    .map(|v| v.rev.as_str());
                if current != Some(parent_rev) {
                    return Err(write_conflict("path_write", "file"));
                }
            }
            let metadata = state.metadata(&key, true);
            state.remove_tree(&key);
            state
                .links
                .retain(|id, _| state.nodes.values().any(|n| &n.id == id));
            Ok(ok(&json!({ "metadata": metadata })))
        }
        "files/move_v2" => {
            let Some(from) = state.resolve(str_arg(arg, "from_path")?)? else {
                return Err(not_found("from_lookup"));
            };
            let to = str_arg(arg, "to_path")?;
            if !to.starts_with('/') || to.ends_with('/') {
                return Err(bad_input("to_path does not match pattern"));
            }
            let to_key = lower(to);
            let same_display = state.nodes.get(&from).is_some_and(|n| n.display == to);
            if to_key == from && same_display {
                return Err(route_error(
                    "duplicated_or_nested_paths",
                    json!({".tag": "duplicated_or_nested_paths"}),
                ));
            }
            if to_key != from && within(&to_key, &from) {
                return Err(route_error(
                    "cant_move_folder_into_itself",
                    json!({".tag": "cant_move_folder_into_itself"}),
                ));
            }
            if to_key != from
                && let Some(node) = state.nodes.get(&to_key)
            {
                let what = if node.folder { "folder" } else { "file" };
                return Err(write_conflict("to", what));
            }
            let display = state.ensure_parents(to, "to")?;
            let key = state.move_tree(&from, &display);
            Ok(ok(&json!({"metadata": state.metadata(&key, true)})))
        }
        "files/list_revisions" => {
            let path = str_arg(arg, "path")?;
            let by_id = arg.get("mode").and_then(Value::as_str) == Some("id");
            let limit = arg
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|l| usize::try_from(l).ok())
                .unwrap_or(10);
            let id = if by_id {
                if !path.starts_with("id:") {
                    return Err(bad_input("mode id needs an id"));
                }
                path.to_owned()
            } else {
                match state.resolve(path)? {
                    Some(key) => state
                        .nodes
                        .get(&key)
                        .map(|n| n.id.clone())
                        .unwrap_or_default(),
                    None => match state.tombstones.get(&lower(path)) {
                        Some(tomb) if !tomb.folder => tomb.id.clone(),
                        Some(_) => {
                            return Err(route_error(
                                "path/not_file",
                                json!({".tag": "path", "path": {".tag": "not_file"}}),
                            ));
                        }
                        None => return Err(not_found("path")),
                    },
                }
            };
            if state
                .key_of_id(&id)
                .is_some_and(|key| state.nodes.get(&key).is_some_and(|n| n.folder))
            {
                return Err(route_error(
                    "path/not_file",
                    json!({".tag": "path", "path": {".tag": "not_file"}}),
                ));
            }
            let Some(versions) = state.history.get(&id) else {
                return Err(not_found("path"));
            };
            let display = state.display_of_id(&id).unwrap_or_default();
            let entries: Vec<Value> = versions
                .iter()
                .rev()
                .take(limit)
                .map(|v| State::version_metadata(&id, &display, v))
                .collect();
            let deleted = state.key_of_id(&id).is_none();
            Ok(ok(&json!({"is_deleted": deleted, "entries": entries})))
        }
        "files/restore" => {
            let path = str_arg(arg, "path")?;
            let rev = str_arg(arg, "rev")?;
            if !path.starts_with('/') {
                return Err(bad_input("path does not match pattern"));
            }
            if !is_rev(rev) {
                return Err(bad_input("rev does not match pattern"));
            }
            let key = lower(path);
            let id = state
                .nodes
                .get(&key)
                .map(|n| n.id.clone())
                .or_else(|| state.tombstones.get(&key).map(|t| t.id.clone()));
            let Some(version) = id
                .as_ref()
                .and_then(|id| state.history.get(id))
                .and_then(|versions| versions.iter().find(|v| v.rev == rev).cloned())
            else {
                return Err(route_error(
                    "invalid_revision",
                    json!({".tag": "invalid_revision"}),
                ));
            };
            if state.nodes.get(&key).is_some_and(|n| n.folder) {
                return Err(write_conflict("path_write", "folder"));
            }
            let display = state
                .nodes
                .get(&key)
                .map(|n| n.display.clone())
                .or_else(|| state.tombstones.get(&key).map(|t| t.display.clone()))
                .unwrap_or_else(|| path.to_owned());
            let display = state.ensure_parents(&display, "path_write")?;
            let key = state.write_file(&display, version.bytes.to_vec());
            Ok(ok(&state.metadata(&key, false)))
        }
        "sharing/create_shared_link_with_settings" => {
            let Some(key) = state.resolve(str_arg(arg, "path")?)? else {
                return Err(not_found("path"));
            };
            let (id, display) = state
                .nodes
                .get(&key)
                .map(|n| (n.id.clone(), n.display.clone()))
                .unwrap_or_default();
            let link = |url: &str| {
                json!({".tag": "file", "url": url, "name": name_of(&display), "id": id,
                       "path_lower": key, "link_permissions": {}})
            };
            if let Some(url) = state.links.get(&id) {
                return Err(route_error(
                    "shared_link_already_exists",
                    json!({".tag": "shared_link_already_exists",
                           "shared_link_already_exists": {".tag": "metadata", "metadata": link(url)}}),
                ));
            }
            let url = format!(
                "https://www.dropbox.com/scl/fi/fake{}/{}?dl=0",
                state.links.len() + 1,
                name_of(&display)
            );
            state.links.insert(id.clone(), url.clone());
            Ok(ok(&link(&url)))
        }
        "sharing/list_shared_links" => {
            let links: Vec<Value> = match arg.get("path").and_then(Value::as_str) {
                Some(path) => {
                    let key = state.resolve(path)?;
                    let id = key.and_then(|k| state.nodes.get(&k).map(|n| n.id.clone()));
                    id.and_then(|id| state.links.get(&id).cloned())
                        .map(|url| json!({"url": url}))
                        .into_iter()
                        .collect()
                }
                None => state
                    .links
                    .values()
                    .map(|url| json!({"url": url}))
                    .collect(),
            };
            Ok(ok(&json!({"links": links, "has_more": false})))
        }
        "users/get_space_usage" => {
            let used: usize = state
                .nodes
                .values()
                .filter_map(|n| n.current.as_ref())
                .map(|v| v.bytes.len())
                .sum();
            Ok(ok(&json!({
                "used": used,
                "allocation": {".tag": "individual", "allocated": state.quota},
            })))
        }
        "users/get_current_account" => {
            let (tag, root) = if state.team {
                ("team", TEAM_NAMESPACE)
            } else {
                ("user", HOME_NAMESPACE)
            };
            Ok(ok(&json!({
                "account_id": "dbid:fake-account",
                "name": {"display_name": "Ada Lovelace"},
                "email": "ada@example.org",
                "root_info": {".tag": tag, "root_namespace_id": root,
                              "home_namespace_id": HOME_NAMESPACE},
            })))
        }
        "files/download" => download(state, arg, request, fault),
        "files/upload" => {
            if request.body.len() > MAX_UPLOAD {
                return Err(bad_input("body too large"));
            }
            check_hash(arg, &request.body)?;
            let metadata = commit(state, arg, request.body.clone())?;
            Ok(ok(&metadata))
        }
        "files/upload_session/start" => {
            if request.body.len() > MAX_UPLOAD {
                return Err(bad_input("body too large"));
            }
            check_hash(arg, &request.body)?;
            let id = format!("pid_upload_session:fake{}", state.bump());
            state.sessions.insert(
                id.clone(),
                Session {
                    bytes: request.body.clone(),
                    closed: arg.get("close").and_then(Value::as_bool).unwrap_or(false),
                },
            );
            Ok(ok(&json!({ "session_id": id })))
        }
        "files/upload_session/append_v2" => {
            let (id, offset) = session_cursor(arg)?;
            check_hash(arg, &request.body)?;
            let Some(session) = state.sessions.get_mut(&id) else {
                return Err(route_error("not_found", json!({".tag": "not_found"})));
            };
            if session.closed {
                return Err(route_error("closed", json!({".tag": "closed"})));
            }
            let len = session.bytes.len() as u64;
            if offset != len {
                return Err(route_error(
                    "incorrect_offset",
                    json!({".tag": "incorrect_offset", "correct_offset": len}),
                ));
            }
            session.bytes.extend_from_slice(&request.body);
            session.closed = arg.get("close").and_then(Value::as_bool).unwrap_or(false);
            Ok(ok(&Value::Null))
        }
        "files/upload_session/finish" => {
            let (id, offset) = session_cursor(arg)?;
            let Some(session) = state.sessions.get(&id) else {
                return Err(route_error(
                    "lookup_failed/not_found",
                    json!({".tag": "lookup_failed", "lookup_failed": {".tag": "not_found"}}),
                ));
            };
            let len = session.bytes.len() as u64;
            if offset != len {
                return Err(route_error(
                    "lookup_failed/incorrect_offset",
                    json!({".tag": "lookup_failed", "lookup_failed":
                           {".tag": "incorrect_offset", "correct_offset": len}}),
                ));
            }
            let mut bytes = session.bytes.clone();
            bytes.extend_from_slice(&request.body);
            check_hash(arg, &bytes)?;
            let commit_arg = arg
                .get("commit")
                .cloned()
                .ok_or_else(|| bad_input("missing commit"))?;
            let metadata = commit(state, &commit_arg, bytes)?;
            state.sessions.remove(&id);
            Ok(ok(&metadata))
        }
        _ => Err(Reply::status(404)),
    }
}

fn session_cursor(arg: &Value) -> Result<(String, u64), Reply> {
    let cursor = arg
        .get("cursor")
        .ok_or_else(|| bad_input("missing cursor"))?;
    let id = str_arg(cursor, "session_id")?.to_owned();
    let offset = cursor
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_input("missing offset"))?;
    Ok((id, offset))
}

fn check_hash(arg: &Value, body: &[u8]) -> Result<(), Reply> {
    match arg.get("content_hash").and_then(Value::as_str) {
        Some(hash) if hash != content_hash(body) => Err(route_error(
            "content_hash_mismatch",
            json!({".tag": "content_hash_mismatch"}),
        )),
        _ => Ok(()),
    }
}

/// Commit `bytes` under a `CommitInfo`: path, `WriteMode`, `strict_conflict`.
fn commit(state: &mut State, arg: &Value, bytes: Vec<u8>) -> Result<Value, Reply> {
    let path = str_arg(arg, "path")?;
    let (key, display) = if path.starts_with("id:") {
        let Some(key) = state.key_of_id(path) else {
            return Err(not_found("path"));
        };
        let display = state
            .nodes
            .get(&key)
            .map(|n| n.display.clone())
            .unwrap_or_default();
        (key, display)
    } else if path.starts_with('/') && !path.ends_with('/') {
        (lower(path), path.to_owned())
    } else {
        return Err(bad_input("path does not match pattern"));
    };
    let strict = arg
        .get("strict_conflict")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mode = arg.get("mode").cloned().unwrap_or_else(|| json!("add"));
    let tag = mode
        .as_str()
        .or_else(|| mode.get(".tag").and_then(Value::as_str))
        .unwrap_or("add");
    let existing = state.nodes.get(&key);
    if existing.is_some_and(|n| n.folder) {
        return Err(upload_conflict("folder"));
    }
    let current = existing.and_then(|n| n.current.clone());
    let identical = current.as_ref().is_some_and(|v| *v.bytes == bytes);
    match tag {
        "add" => {
            if current.is_some() {
                if !strict && identical {
                    return Ok(state.metadata(&key, false));
                }
                return Err(upload_conflict("file"));
            }
        }
        "overwrite" => {}
        "update" => {
            let rev = mode
                .get("update")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !is_rev(rev) {
                return Err(bad_input("rev does not match pattern"));
            }
            match &current {
                None => return Err(upload_conflict("file")),
                Some(version) if version.rev != rev => {
                    if !strict && identical {
                        return Ok(state.metadata(&key, false));
                    }
                    return Err(upload_conflict("file"));
                }
                Some(_) => {}
            }
        }
        _ => return Err(bad_input("unknown write mode")),
    }
    let display = state.ensure_parents(&display, "path")?;
    let key = state.write_file(&display, bytes);
    Ok(state.metadata(&key, false))
}

fn parse_range(header: &str, len: u64) -> Option<std::ops::Range<u64>> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        return Some(len.saturating_sub(suffix)..len);
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        len
    } else {
        end.parse::<u64>().ok()?.saturating_add(1).min(len)
    };
    Some(start..end)
}

fn download(
    state: &State,
    arg: &Value,
    request: &Request,
    fault: Option<FakeFault>,
) -> Result<Reply, Reply> {
    let path = str_arg(arg, "path")?;
    let (metadata, bytes) = if let Some(rev) = path.strip_prefix("rev:") {
        let Some((id, version)) = state.find_version(rev) else {
            return Err(not_found("path"));
        };
        let display = state.display_of_id(&id).unwrap_or_default();
        (
            State::version_metadata(&id, &display, &version),
            version.bytes,
        )
    } else {
        let Some(key) = state.resolve(path)? else {
            return Err(not_found("path"));
        };
        let node = state.nodes.get(&key).ok_or_else(|| not_found("path"))?;
        let Some(version) = &node.current else {
            return Err(route_error(
                "path/not_file",
                json!({".tag": "path", "path": {".tag": "not_file"}}),
            ));
        };
        (state.metadata(&key, false), Arc::clone(&version.bytes))
    };
    let len = bytes.len() as u64;
    let result = crate::api::ascii_json(&metadata);
    let (reply, mut body) = match request.header("range") {
        Some(range) => {
            let Some(range) = parse_range(range, len).filter(|r| r.start < len && r.start < r.end)
            else {
                return Err(Reply::status(416).header("Content-Range", &format!("bytes */{len}")));
            };
            let start = usize::try_from(range.start).unwrap_or(usize::MAX);
            let end = usize::try_from(range.end).unwrap_or(usize::MAX);
            (
                Reply::status(206).header(
                    "Content-Range",
                    &format!("bytes {}-{}/{len}", range.start, range.end - 1),
                ),
                bytes.get(start..end).unwrap_or_default().to_vec(),
            )
        }
        None => (Reply::status(200), bytes.to_vec()),
    };
    let reply = reply
        .header("Content-Type", "application/octet-stream")
        .header("Dropbox-API-Result", &result);
    Ok(match fault {
        Some(FakeFault::TruncatedBody) => reply.body(body).cut_short(true),
        Some(FakeFault::ClosedEarly) => reply.body(body).cut_short(false),
        Some(FakeFault::CorruptBody) => {
            let middle = body.len() / 2;
            if let Some(byte) = body.get_mut(middle) {
                *byte ^= 0x5a;
            }
            reply.body(body)
        }
        _ => reply.body(body),
    })
}
