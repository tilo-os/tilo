//! An in-memory `WebDAV`/Nextcloud server on the shared test responder.
//!
//! Enough of RFC 4918 (PROPFIND, GET with ranges and `If-Match`, PUT with
//! `If-Match`/`If-None-Match`, MKCOL, MOVE with `Overwrite`, DELETE), RFC
//! 6578 `sync-collection` (optional) and Nextcloud's chunked uploads,
//! versions and recursive folder `ETag`s to exercise a real client. Faults
//! can be injected into the next request, and the server can go offline
//! (connections are dropped unanswered).
//!
//! Test support only (feature `fake-server`).

#![allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "a test fixture whose lock is poisoned has already failed the test"
)]

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use test_http::{Reply, Request, Responder};
use tilo_dav::Url;

use crate::paths::{format_http_date, percent_decode};
use crate::{DavConfig, Flavor};

/// The login the fake accepts.
pub const USER: &str = "ada";
pub const PASSWORD: &str = "correct horse";

/// A fault for the next matching request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFault {
    /// 429 with `Retry-After: 1`.
    RateLimited,
    /// 503.
    ServerError,
    /// The next GET declares its full length but sends half the body.
    TruncatedBody,
    /// 401 once.
    ExpiredCredential,
}

/// Request counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub propfinds: usize,
    pub ranged_gets: usize,
    pub full_gets: usize,
    pub puts: usize,
    pub bytes_served: u64,
}

#[derive(Clone, Debug)]
struct Version {
    name: String,
    bytes: Vec<u8>,
    modified: SystemTime,
}

#[derive(Clone, Debug)]
struct Node {
    folder: bool,
    bytes: Vec<u8>,
    etag: String,
    modified: SystemTime,
    fileid: u64,
    versions: Vec<Version>,
}

#[allow(clippy::struct_excessive_bools, reason = "independent server switches")]
struct State {
    nextcloud: bool,
    sync: bool,
    offline: bool,
    reject: bool,
    nodes: BTreeMap<String, Node>,
    seq: u64,
    log: Vec<(u64, String)>,
    log_floor: u64,
    faults: VecDeque<FakeFault>,
    uploads: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    stats: Stats,
}

/// A running fake server. Dropping it stops the server.
pub struct FakeDav {
    responder: Responder,
    state: Arc<Mutex<State>>,
    nextcloud: bool,
}

const FILES_PREFIX: &str = "/remote.php/dav/files/ada/";
const UPLOADS_PREFIX: &str = "/remote.php/dav/uploads/ada/";
const VERSIONS_PREFIX: &str = "/remote.php/dav/versions/ada/versions/";
const RESTORE_TARGET: &str = "/remote.php/dav/versions/ada/restore/target";
const GENERIC_PREFIX: &str = "/dav/";

impl FakeDav {
    /// A plain `WebDAV` server rooted at `/dav/`.
    pub fn generic() -> Self {
        Self::start(false, false)
    }

    /// A Nextcloud server for user [`USER`].
    pub fn nextcloud() -> Self {
        Self::start(true, false)
    }

    /// A plain `WebDAV` server that also answers `sync-collection`.
    pub fn generic_with_sync_collection() -> Self {
        Self::start(false, true)
    }

    fn start(nextcloud: bool, sync: bool) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            String::new(),
            Node {
                folder: true,
                bytes: Vec::new(),
                etag: "\"0\"".into(),
                modified: SystemTime::now(),
                fileid: 1,
                versions: Vec::new(),
            },
        );
        let state = Arc::new(Mutex::new(State {
            nextcloud,
            sync,
            offline: false,
            reject: false,
            nodes,
            seq: 1,
            log: Vec::new(),
            log_floor: 0,
            faults: VecDeque::new(),
            uploads: BTreeMap::new(),
            stats: Stats::default(),
        }));
        let shared = Arc::clone(&state);
        let responder = Responder::handle(move |request| {
            let mut state = shared.lock().expect("fake state");
            handle(&mut state, request)
        });
        Self {
            responder,
            state,
            nextcloud,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }

    /// `http://127.0.0.1:<port>/`: the `server_url` of a Nextcloud account.
    pub fn server_url(&self) -> Url {
        Url::parse(&format!("{}/", self.responder.url())).expect("fake url")
    }

    /// The collection URL of the account root.
    pub fn root_url(&self) -> Url {
        let prefix = if self.nextcloud {
            FILES_PREFIX
        } else {
            GENERIC_PREFIX
        };
        Url::parse(&format!("{}{prefix}", self.responder.url())).expect("fake url")
    }

    /// A mount configuration for this server, with 64 KiB upload chunks so
    /// tests exercise chunking cheaply.
    pub fn config(&self) -> DavConfig {
        let mut config = if self.nextcloud {
            DavConfig::nextcloud(self.server_url(), USER).expect("nextcloud config")
        } else {
            DavConfig::generic(self.root_url())
        };
        config.upload_chunk = 64 * 1024;
        config
    }

    /// The account JSON Settings would store for this server.
    pub fn account_config(&self) -> serde_json::Value {
        if self.nextcloud {
            serde_json::json!({"server_url": self.server_url().as_str(), "username": USER})
        } else {
            serde_json::json!({"server_url": self.root_url().as_str(), "username": USER})
        }
    }

    pub fn is_nextcloud(&self) -> bool {
        matches!(self.config().flavor, Flavor::Nextcloud { .. })
    }

    /// Create or replace a file, creating parent folders.
    pub fn put(&self, path: &str, bytes: &[u8]) {
        let mut state = self.lock();
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let mut prefix = String::new();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            prefix = join(&prefix, part);
            if !state.nodes.contains_key(&prefix) {
                state.create(&prefix, true, Vec::new());
            }
        }
        state.write(&parts.join("/"), bytes.to_vec());
    }

    /// Create a folder (and its parents).
    pub fn mkdir(&self, path: &str) {
        let mut state = self.lock();
        let mut prefix = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            prefix = join(&prefix, part);
            if !state.nodes.contains_key(&prefix) {
                state.create(&prefix, true, Vec::new());
            }
        }
    }

    /// Delete an item and everything below it.
    pub fn remove(&self, path: &str) {
        self.lock().remove_tree(path.trim_matches('/'));
    }

    /// A file's current bytes.
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.lock()
            .nodes
            .get(path.trim_matches('/'))
            .filter(|n| !n.folder)
            .map(|n| n.bytes.clone())
    }

    /// Every path on the server, sorted (the root is omitted).
    pub fn paths(&self) -> Vec<String> {
        self.lock()
            .nodes
            .keys()
            .filter(|k| !k.is_empty())
            .cloned()
            .collect()
    }

    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    pub fn set_reject(&self, reject: bool) {
        self.lock().reject = reject;
    }

    pub fn inject(&self, fault: FakeFault) {
        self.lock().faults.push_back(fault);
    }

    /// Forget every issued sync token.
    pub fn reset_cursors(&self) {
        let mut state = self.lock();
        state.log_floor = state.seq.saturating_add(1);
    }

    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// Requests received since the last call.
    pub fn requests(&self) -> Vec<Request> {
        self.responder.drain_requests()
    }

    /// Block until a request matching `wanted` has been handled (its effect
    /// is visible), skipping others. Panics if requests stop arriving.
    pub fn wait_for(&self, wanted: impl Fn(&Request) -> bool) -> Request {
        loop {
            let request = self.responder.next_request();
            if wanted(&request) {
                return request;
            }
        }
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn whole_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| percent_encoding::utf8_percent_encode(segment, UNRESERVED).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Everything but the RFC 3986 unreserved characters.
const UNRESERVED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn decode_path(raw: &str) -> Option<String> {
    let parts: Option<Vec<String>> = raw
        .split('/')
        .filter(|p| !p.is_empty())
        .map(percent_decode)
        .collect();
    Some(parts?.join("/"))
}

fn base64(input: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, input)
}

impl State {
    fn bump(&mut self) -> String {
        self.seq = self.seq.saturating_add(1);
        format!("\"{}\"", self.seq)
    }

    fn log(&mut self, path: &str) {
        let seq = self.seq;
        self.log.push((seq, path.to_owned()));
        if self.nextcloud {
            // Nextcloud folder ETags change with their contents.
            let mut current = parent(path).to_owned();
            loop {
                let etag = format!("\"{}d\"", self.seq);
                if let Some(node) = self.nodes.get_mut(&current) {
                    node.etag = etag;
                }
                if current.is_empty() {
                    break;
                }
                current = parent(&current).to_owned();
            }
        }
    }

    fn create(&mut self, path: &str, folder: bool, bytes: Vec<u8>) {
        let etag = self.bump();
        let fileid = self.seq;
        self.nodes.insert(
            path.to_owned(),
            Node {
                folder,
                bytes,
                etag,
                modified: SystemTime::now(),
                fileid,
                versions: Vec::new(),
            },
        );
        self.log(path);
    }

    fn write(&mut self, path: &str, bytes: Vec<u8>) {
        self.write_with(path, bytes, false);
    }

    /// A write through the files endpoint (`PUT`, or a chunked upload's
    /// assembly). Nextcloud's scanner then keeps the file's `ETag` while its
    /// whole-second storage mtime is unchanged, even across an overwrite.
    /// Out-of-band writes ([`FakeDav::put`], version restores) always get a
    /// new `ETag`, as a writer in another second would.
    fn upload(&mut self, path: &str, bytes: Vec<u8>) {
        self.write_with(path, bytes, true);
    }

    fn write_with(&mut self, path: &str, bytes: Vec<u8>, same_second_reuse: bool) {
        let etag = self.bump();
        let nextcloud = self.nextcloud;
        if let Some(node) = self.nodes.get_mut(path) {
            if nextcloud {
                node.versions.push(Version {
                    name: format!("{}", node.versions.len().saturating_add(1)),
                    bytes: std::mem::take(&mut node.bytes),
                    modified: node.modified,
                });
            }
            let now = SystemTime::now();
            node.bytes = bytes;
            if !(nextcloud
                && same_second_reuse
                && whole_seconds(node.modified) == whole_seconds(now))
            {
                node.etag = etag;
            }
            node.modified = now;
            self.log(path);
        } else {
            self.seq = self.seq.saturating_sub(1);
            self.create(path, false, bytes);
        }
    }

    fn remove_tree(&mut self, path: &str) {
        let doomed: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| *k == path || k.starts_with(&format!("{path}/")))
            .cloned()
            .collect();
        if doomed.is_empty() {
            return;
        }
        self.bump();
        for key in doomed {
            self.nodes.remove(&key);
            self.log(&key);
        }
    }

    fn move_tree(&mut self, from: &str, to: &str) {
        let moving: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| *k == from || k.starts_with(&format!("{from}/")))
            .cloned()
            .collect();
        self.bump();
        for key in moving {
            if let Some(node) = self.nodes.remove(&key) {
                let rest = key.get(from.len()..).unwrap_or("");
                let target = format!("{to}{rest}");
                self.log(&key);
                self.nodes.insert(target.clone(), node);
                self.log(&target);
            }
        }
    }

    fn propstat(&self, path: &str, node: &Node, href_prefix: &str) -> String {
        let href = if path.is_empty() {
            href_prefix.to_owned()
        } else if node.folder {
            format!("{href_prefix}{}/", encode_path(path))
        } else {
            format!("{href_prefix}{}", encode_path(path))
        };
        let mut props = format!(
            "<d:getetag>{}</d:getetag><d:getlastmodified>{}</d:getlastmodified>",
            node.etag,
            format_http_date(node.modified)
        );
        if node.folder {
            props.push_str("<d:resourcetype><d:collection/></d:resourcetype>");
        } else {
            let _ = write!(
                props,
                "<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength><d:getcontenttype>application/octet-stream</d:getcontenttype>",
                node.bytes.len()
            );
        }
        if self.nextcloud {
            let _ = write!(props, "<oc:fileid>{}</oc:fileid>", node.fileid);
        }
        if path.is_empty() {
            let used: usize = self.nodes.values().map(|n| n.bytes.len()).sum();
            let _ = write!(
                props,
                "<d:quota-used-bytes>{used}</d:quota-used-bytes><d:quota-available-bytes>1000000000</d:quota-available-bytes>"
            );
        }
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        )
    }
}

fn multistatus(body: &str, token: Option<&str>) -> Reply {
    let token = token
        .map(|t| format!("<d:sync-token>{t}</d:sync-token>"))
        .unwrap_or_default();
    Reply::status(207)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(format!(
            "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\">{body}{token}</d:multistatus>"
        ))
}

fn etag_matches(request: &Request, node: Option<&Node>) -> bool {
    if let Some(expected) = request.header("if-match") {
        return match node {
            Some(node) => expected == "*" || expected == node.etag,
            None => false,
        };
    }
    if request.header("if-none-match") == Some("*") {
        return node.is_none();
    }
    true
}

/// A chunked upload's assembly `MOVE`, as Nextcloud evaluates it. Plain
/// `If-Match`/`If-None-Match` test the request URI, the virtual `.file`,
/// which exists and has no etag a client knows. The destination is tested
/// by `Overwrite: F` and by an `If` header tagged with its URL.
fn assembly_condition_holds(request: &Request, destination: Option<&Node>) -> bool {
    if request.header("if-none-match") == Some("*") {
        return false;
    }
    if request
        .header("if-match")
        .is_some_and(|expected| expected != "*")
    {
        return false;
    }
    if request.header("overwrite") == Some("F") && destination.is_some() {
        return false;
    }
    match request.header("if") {
        Some(tagged) => {
            let expected = tagged
                .split_once("([")
                .and_then(|(_, rest)| rest.split_once("])"))
                .map(|(etag, _)| etag);
            matches!((expected, destination), (Some(etag), Some(node)) if etag == node.etag)
        }
        None => true,
    }
}

fn handle(state: &mut State, request: &Request) -> Option<Reply> {
    if state.offline {
        return None;
    }
    let expected = format!("Basic {}", base64(format!("{USER}:{PASSWORD}").as_bytes()));
    if state.reject || request.header("authorization") != Some(expected.as_str()) {
        return Some(Reply::status(401).header("WWW-Authenticate", "Basic realm=\"fake\""));
    }
    let truncate = match state.faults.pop_front() {
        Some(FakeFault::RateLimited) => {
            return Some(Reply::status(429).header("Retry-After", "1"));
        }
        Some(FakeFault::ServerError) => return Some(Reply::status(503)),
        Some(FakeFault::ExpiredCredential) => return Some(Reply::status(401)),
        Some(FakeFault::TruncatedBody) => true,
        None => false,
    };
    let target = request.target.split('?').next().unwrap_or("");
    let (area, rest) = if state.nextcloud {
        if let Some(rest) = target.strip_prefix(FILES_PREFIX) {
            ("files", rest)
        } else if target == FILES_PREFIX.trim_end_matches('/') {
            ("files", "")
        } else if let Some(rest) = target.strip_prefix(UPLOADS_PREFIX) {
            ("uploads", rest)
        } else if let Some(rest) = target.strip_prefix(VERSIONS_PREFIX) {
            ("versions", rest)
        } else {
            return Some(Reply::status(404));
        }
    } else if let Some(rest) = target.strip_prefix(GENERIC_PREFIX) {
        ("files", rest)
    } else if target == GENERIC_PREFIX.trim_end_matches('/') {
        ("files", "")
    } else {
        return Some(Reply::status(404));
    };
    let path = decode_path(rest)?;
    Some(match area {
        "uploads" => uploads(state, request, &path),
        "versions" => versions(state, request, &path),
        _ => files(state, request, &path, truncate),
    })
}

fn files_prefix(state: &State) -> &'static str {
    if state.nextcloud {
        FILES_PREFIX
    } else {
        GENERIC_PREFIX
    }
}

/// The files-area path of a `Destination` header.
fn destination(state: &State, request: &Request) -> Option<String> {
    let raw = request.header("destination")?;
    let url = Url::parse(raw).ok()?;
    let rest = url.path().strip_prefix(files_prefix(state))?;
    decode_path(rest)
}

#[allow(
    clippy::too_many_lines,
    reason = "one match over the WebDAV methods reads best in one place"
)]
fn files(state: &mut State, request: &Request, path: &str, truncate: bool) -> Reply {
    let prefix = files_prefix(state);
    match request.method.as_str() {
        "PROPFIND" => {
            state.stats.propfinds = state.stats.propfinds.saturating_add(1);
            let Some(node) = state.nodes.get(path) else {
                return Reply::status(404);
            };
            // Like Apache `mod_dav` behind `DirectorySlash`, the plain server
            // redirects a folder's bare path to its slashed one.
            let target = request.target.split('?').next().unwrap_or("");
            if !state.nextcloud && node.folder && !path.is_empty() && !target.ends_with('/') {
                return Reply::status(301).header("Location", &format!("{target}/"));
            }
            let mut body = state.propstat(path, node, prefix);
            if node.folder && request.header("depth") != Some("0") {
                let children: Vec<(&String, &Node)> = state
                    .nodes
                    .iter()
                    .filter(|(k, _)| !k.is_empty() && *k != path && parent(k) == path)
                    .collect();
                for (key, child) in children {
                    body.push_str(&state.propstat(key, child, prefix));
                }
            }
            multistatus(&body, None)
        }
        "GET" => {
            let Some(node) = state.nodes.get(path) else {
                return Reply::status(404);
            };
            if node.folder {
                return Reply::status(405);
            }
            // Like Apache, the plain server marks a file's entity tag weak
            // for the second after it changed, and `If-Match` then fails the
            // strong comparison.
            let fresh = !state.nextcloud
                && !node
                    .modified
                    .elapsed()
                    .is_ok_and(|age| age >= Duration::from_secs(1));
            let etag = if fresh {
                format!("W/{}", node.etag)
            } else {
                node.etag.clone()
            };
            let strong_match_refused = fresh
                && request
                    .header("if-match")
                    .is_some_and(|expected| expected != "*");
            if strong_match_refused || !etag_matches(request, Some(node)) {
                return Reply::status(412).header("ETag", &etag);
            }
            let bytes = node.bytes.clone();
            let total = bytes.len();
            let range = request
                .header("range")
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.split_once('-'))
                .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
            let (status, slice, content_range) = match range {
                Some((start, end)) if start < total => {
                    state.stats.ranged_gets = state.stats.ranged_gets.saturating_add(1);
                    let end = end.min(total.saturating_sub(1));
                    (
                        206,
                        bytes.get(start..=end).unwrap_or_default().to_vec(),
                        Some(format!("bytes {start}-{end}/{total}")),
                    )
                }
                Some(_) => return Reply::status(416),
                None => {
                    state.stats.full_gets = state.stats.full_gets.saturating_add(1);
                    (200, bytes, None)
                }
            };
            state.stats.bytes_served = state
                .stats
                .bytes_served
                .saturating_add(u64::try_from(slice.len()).unwrap_or(0));
            let mut reply = Reply::status(status).header("ETag", &etag);
            if let Some(range) = content_range {
                reply = reply.header("Content-Range", &range);
            }
            if truncate {
                let half = slice.len().checked_div(2).unwrap_or(0);
                reply
                    .header("Content-Length", &slice.len().to_string())
                    .body(slice.get(..half).unwrap_or_default().to_vec())
                    .without_content_length()
            } else {
                reply.body(slice)
            }
        }
        "PUT" => {
            state.stats.puts = state.stats.puts.saturating_add(1);
            if !state.nodes.get(parent(path)).is_some_and(|n| n.folder) {
                return Reply::status(409);
            }
            let existing = state.nodes.get(path);
            if existing.is_some_and(|n| n.folder) {
                return Reply::status(405);
            }
            if !etag_matches(request, existing) {
                return Reply::status(412);
            }
            let created = existing.is_none();
            state.upload(path, request.body.clone());
            let etag = state
                .nodes
                .get(path)
                .map(|n| n.etag.clone())
                .unwrap_or_default();
            Reply::status(if created { 201 } else { 204 }).header("ETag", &etag)
        }
        "MKCOL" => {
            if state.nodes.contains_key(path) {
                return Reply::status(405);
            }
            if !state.nodes.get(parent(path)).is_some_and(|n| n.folder) {
                return Reply::status(409);
            }
            state.create(path, true, Vec::new());
            Reply::status(201)
        }
        "MOVE" => {
            let Some(to) = destination(state, request) else {
                return Reply::status(400);
            };
            let Some(node) = state.nodes.get(path) else {
                return Reply::status(404);
            };
            if !etag_matches(request, Some(node)) {
                return Reply::status(412);
            }
            let overwrite = request.header("overwrite") != Some("F");
            let exists = state.nodes.contains_key(&to);
            if exists && !overwrite {
                return Reply::status(412);
            }
            if !state.nodes.get(parent(&to)).is_some_and(|n| n.folder) {
                return Reply::status(409);
            }
            if exists {
                state.remove_tree(&to);
            }
            state.move_tree(path, &to);
            Reply::status(if exists { 204 } else { 201 })
        }
        "DELETE" => {
            let Some(node) = state.nodes.get(path) else {
                return Reply::status(404);
            };
            if path.is_empty() {
                return Reply::status(403);
            }
            if !etag_matches(request, Some(node)) {
                return Reply::status(412);
            }
            state.remove_tree(path);
            Reply::status(204)
        }
        "REPORT" => sync_collection(state, request),
        // Nextcloud sets a file's mtime from `{DAV:}lastmodified`, and a
        // changed mtime gives the file a new ETag.
        "PROPPATCH" if state.nextcloud => {
            let Some(node) = state.nodes.get(path) else {
                return Reply::status(404);
            };
            if !etag_matches(request, Some(node)) {
                return Reply::status(412);
            }
            let body = request.body_text();
            let Some(secs) = body
                .split_once("<d:lastmodified>")
                .and_then(|(_, rest)| rest.split_once("</d:lastmodified>"))
                .and_then(|(secs, _)| secs.trim().parse::<u64>().ok())
            else {
                return Reply::status(400);
            };
            let etag = state.bump();
            if let Some(node) = state.nodes.get_mut(path) {
                node.modified = SystemTime::UNIX_EPOCH
                    .checked_add(Duration::from_secs(secs))
                    .unwrap_or(node.modified);
                node.etag = etag;
            }
            state.log(path);
            let body = state
                .nodes
                .get(path)
                .map(|node| state.propstat(path, node, prefix))
                .unwrap_or_default();
            multistatus(&body, None)
        }
        _ => Reply::status(405),
    }
}

fn sync_collection(state: &State, request: &Request) -> Reply {
    if !state.sync {
        return Reply::status(501);
    }
    let body = request.body_text();
    let token = body
        .split_once("<D:sync-token>")
        .and_then(|(_, rest)| rest.split_once("</D:sync-token>"))
        .map_or("", |(token, _)| token);
    let prefix = files_prefix(state);
    let mut out = String::new();
    if token.is_empty() {
        for (key, node) in state.nodes.iter().filter(|(k, _)| !k.is_empty()) {
            out.push_str(&state.propstat(key, node, prefix));
        }
    } else {
        let Some(since) = token
            .strip_prefix("fake-")
            .and_then(|t| t.parse::<u64>().ok())
        else {
            return Reply::status(403);
        };
        if since < state.log_floor {
            return Reply::status(403);
        }
        let mut changed: Vec<&String> = state
            .log
            .iter()
            .filter(|(seq, _)| *seq > since)
            .map(|(_, path)| path)
            .collect();
        changed.sort();
        changed.dedup();
        for path in changed {
            match state.nodes.get(path) {
                Some(node) => out.push_str(&state.propstat(path, node, prefix)),
                None => {
                    let _ = write!(
                        out,
                        "<d:response><d:href>{prefix}{}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>",
                        encode_path(path)
                    );
                }
            }
        }
    }
    let token = format!("fake-{}", state.seq);
    multistatus(&out, Some(&token))
}

fn uploads(state: &mut State, request: &Request, path: &str) -> Reply {
    let (transfer, chunk) = match path.split_once('/') {
        Some((transfer, chunk)) => (transfer.to_owned(), Some(chunk.to_owned())),
        None => (path.to_owned(), None),
    };
    match (request.method.as_str(), chunk) {
        ("MKCOL", None) => {
            state.uploads.insert(transfer, BTreeMap::new());
            Reply::status(201)
        }
        ("PUT", Some(chunk)) => match state.uploads.get_mut(&transfer) {
            Some(chunks) => {
                chunks.insert(chunk, request.body.clone());
                Reply::status(201)
            }
            None => Reply::status(404),
        },
        ("PROPFIND", None) => {
            let Some(chunks) = state.uploads.get(&transfer) else {
                return Reply::status(404);
            };
            let base = format!("{UPLOADS_PREFIX}{transfer}/");
            let mut body = format!(
                "<d:response><d:href>{base}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            );
            for (name, bytes) in chunks {
                let _ = write!(
                    body,
                    "<d:response><d:href>{base}{name}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    bytes.len()
                );
            }
            multistatus(&body, None)
        }
        ("MOVE", Some(file)) if file == ".file" => {
            let Some(to) = destination(state, request) else {
                return Reply::status(400);
            };
            if !state.uploads.contains_key(&transfer) {
                return Reply::status(404);
            }
            if !assembly_condition_holds(request, state.nodes.get(&to)) {
                return Reply::status(412);
            }
            if !state.nodes.get(parent(&to)).is_some_and(|n| n.folder) {
                return Reply::status(409);
            }
            let chunks = state.uploads.remove(&transfer).unwrap_or_default();
            let bytes: Vec<u8> = chunks.into_values().flatten().collect();
            let created = !state.nodes.contains_key(&to);
            state.upload(&to, bytes);
            let etag = state
                .nodes
                .get(&to)
                .map(|n| n.etag.clone())
                .unwrap_or_default();
            Reply::status(if created { 201 } else { 204 }).header("ETag", &etag)
        }
        ("DELETE", None) => {
            state.uploads.remove(&transfer);
            Reply::status(204)
        }
        _ => Reply::status(405),
    }
}

fn versions(state: &mut State, request: &Request, path: &str) -> Reply {
    let (fileid, version) = match path.split_once('/') {
        Some((id, version)) => (id.to_owned(), Some(version.to_owned())),
        None => (path.to_owned(), None),
    };
    let Some(file_path) = state
        .nodes
        .iter()
        .find(|(_, n)| n.fileid.to_string() == fileid && !n.folder)
        .map(|(k, _)| k.clone())
    else {
        return Reply::status(404);
    };
    let base = format!("{VERSIONS_PREFIX}{fileid}/");
    match (request.method.as_str(), version) {
        ("PROPFIND", None) => {
            let node = state.nodes.get(&file_path);
            let mut body = format!(
                "<d:response><d:href>{base}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            );
            for version in node.map(|n| n.versions.as_slice()).unwrap_or_default() {
                let _ = write!(
                    body,
                    "<d:response><d:href>{base}{}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{}</d:getcontentlength><d:getlastmodified>{}</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    version.name,
                    version.bytes.len(),
                    format_http_date(version.modified)
                );
            }
            multistatus(&body, None)
        }
        ("GET", Some(version)) => state
            .nodes
            .get(&file_path)
            .and_then(|n| n.versions.iter().find(|v| v.name == version))
            .map_or_else(
                || Reply::status(404),
                |v| Reply::status(200).body(v.bytes.clone()),
            ),
        ("MOVE", Some(version)) => {
            let restore = request
                .header("destination")
                .and_then(|d| Url::parse(d).ok())
                .is_some_and(|u| u.path() == RESTORE_TARGET);
            if !restore {
                return Reply::status(400);
            }
            let Some(bytes) = state
                .nodes
                .get(&file_path)
                .and_then(|n| n.versions.iter().find(|v| v.name == version))
                .map(|v| v.bytes.clone())
            else {
                return Reply::status(404);
            };
            state.write(&file_path, bytes);
            Reply::status(204)
        }
        _ => Reply::status(405),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_standard_base64() {
        assert_eq!(base64(b"ada:s3cret"), "YWRhOnMzY3JldA==");
        assert_eq!(base64(b"ab"), "YWI=");
    }
}
