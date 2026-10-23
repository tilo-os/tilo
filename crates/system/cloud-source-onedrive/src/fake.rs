//! An in-memory Microsoft Graph drive on the shared test responder.
//!
//! Enough of the `driveItem` API to exercise a real client, with Graph's
//! semantics rather than a convenient approximation:
//!
//! - items by id and by path below an item (`items/{id}:/{path}:`), names
//!   unique among siblings case-insensitively;
//! - `root/delta` with paging (`@odata.nextLink`), `@odata.deltaLink`,
//!   tombstones with the `deleted` facet, the parent chain of every change,
//!   `410 Gone` with a fresh `Location` for forgotten tokens, and the
//!   per-flavour omissions (no `parentReference.path`; no `cTag` and no name
//!   on tombstones for business drives);
//! - `If-Match` against `eTag` or `cTag` on writes, moves, deletes and upload
//!   session creation (`412`); `conflictBehavior` (`fail`, `replace`,
//!   `rename`) on folder creation, uploads and simple `PUT`s (`409
//!   nameAlreadyExists`);
//! - downloads through pre-authenticated `@microsoft.graph.downloadUrl`s and
//!   `302` redirects, with `Range`; bearer tokens sent there are refused
//!   (and counted);
//! - upload sessions: sequential fragments, 320 KiB multiples except the
//!   last, `416` for a fragment the server already has, commit on the last
//!   byte with the name clash detected at commit, status and cancel;
//! - versions (list newest first, download, `restoreVersion`), the recycle
//!   bin, `createLink`, quota, `OneNote` notebooks (`package`).
//!
//! Faults can be injected into the next request, and the server can go
//! offline (connections are dropped unanswered).
//!
//! Test support only (feature `fake-server`).

#![allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "a test fixture whose lock is poisoned has already failed the test"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "string indexing of serde_json values builds and reads JSON objects; it never panics on objects or null"
)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest as _;
use test_http::{Reply, Request, Responder};

use crate::graph::DriveType;
use crate::paths::{encode_segment, percent_decode};
use crate::quickxor::quick_xor_base64;
use crate::{FRAGMENT_UNIT, OneDriveConfig, RootSelector};

pub const TOKEN: &str = "fake-graph-access-token";
/// Total storage the fake reports.
pub const QUOTA_TOTAL: u64 = 5 * 1024 * 1024 * 1024;
const VERSIONS_KEPT: usize = 25;

/// A fault for the next matching request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFault {
    /// 429 with `Retry-After: 1`.
    RateLimited,
    /// 503.
    ServerError,
    /// The next download declares its full length but sends half the body.
    TruncatedBody,
    /// The next download sends half the body, declares no length and closes
    /// the connection.
    ClosedEarly,
    /// 401 once, as for an expired access token.
    ExpiredCredential,
}

/// Request counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub graph_requests: usize,
    pub delta_requests: usize,
    pub downloads: usize,
    pub ranged_downloads: usize,
    pub fragments: usize,
    pub bytes_served: u64,
    /// Requests to pre-authenticated URLs that carried a bearer token.
    pub bearer_leaks: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Root,
    Folder,
    File,
    Notebook,
}

#[derive(Clone, Debug)]
struct StoredVersion {
    id: String,
    bytes: Vec<u8>,
    modified: SystemTime,
}

#[derive(Clone, Debug)]
struct Node {
    name: String,
    parent: Option<String>,
    kind: Kind,
    bytes: Vec<u8>,
    guid: String,
    etag: u64,
    ctag: u64,
    created: SystemTime,
    modified: SystemTime,
    /// The change sequence number of the last change, for delta.
    seq: u64,
    /// Content history, oldest first; the last entry is the current body.
    versions: Vec<StoredVersion>,
    version_counter: u64,
    link: Option<String>,
}

impl Node {
    fn is_container(&self) -> bool {
        matches!(self.kind, Kind::Root | Kind::Folder | Kind::Notebook)
    }

    fn etag(&self) -> String {
        format!("\"{{{}}},{}\"", self.guid, self.etag)
    }

    fn ctag(&self) -> String {
        format!("\"c:{{{}}},{}\"", self.guid, self.ctag)
    }
}

#[derive(Clone, Debug)]
struct Tombstone {
    id: String,
    parent: Option<String>,
    name: String,
    folder: bool,
    seq: u64,
}

#[derive(Clone, Debug)]
enum SessionTarget {
    Replace(String),
    Create { parent: String, name: String },
}

#[derive(Clone, Debug)]
struct UploadSession {
    target: SessionTarget,
    behavior: String,
    size: u64,
    received: Vec<u8>,
}

struct State {
    flavor: DriveType,
    base: String,
    drive_id: String,
    root: String,
    items: BTreeMap<String, Node>,
    recycle_bin: Vec<(String, Node)>,
    tombstones: Vec<Tombstone>,
    seq: u64,
    epoch: u64,
    next_id: u64,
    faults: VecDeque<FakeFault>,
    offline: bool,
    downloads: HashMap<String, Vec<u8>>,
    uploads: HashMap<String, UploadSession>,
    page_size: usize,
    stats: Stats,
}

/// A running fake drive. Dropping it stops the server.
pub struct FakeGraph {
    responder: Responder,
    state: Arc<Mutex<State>>,
}

impl FakeGraph {
    /// A consumer `OneDrive`.
    pub fn personal() -> Self {
        Self::start(DriveType::Personal)
    }

    /// A `OneDrive` for Business drive.
    pub fn business() -> Self {
        Self::start(DriveType::Business)
    }

    fn start(flavor: DriveType) -> Self {
        let drive_id = match flavor {
            DriveType::Personal => "5d1c0a7f2e9b4c3a".to_owned(),
            DriveType::Business | DriveType::DocumentLibrary => {
                "b!t18F8ybsHUq1z3LTz8xvZqP8zaSWjkFNhsME-Fepo75dTf9vQKfeRblBZjoSQrd7".to_owned()
            }
        };
        let mut state = State {
            flavor,
            base: String::new(),
            drive_id,
            root: String::new(),
            items: BTreeMap::new(),
            recycle_bin: Vec::new(),
            tombstones: Vec::new(),
            seq: 0,
            epoch: 1,
            next_id: 100,
            faults: VecDeque::new(),
            offline: false,
            downloads: HashMap::new(),
            uploads: HashMap::new(),
            page_size: 4,
            stats: Stats::default(),
        };
        let root = state.new_id();
        let now = SystemTime::now();
        state.items.insert(
            root.clone(),
            Node {
                name: "root".into(),
                parent: None,
                kind: Kind::Root,
                bytes: Vec::new(),
                guid: guid_for(&root),
                etag: 1,
                ctag: 1,
                created: now,
                modified: now,
                seq: 0,
                versions: Vec::new(),
                version_counter: 0,
                link: None,
            },
        );
        state.root.clone_from(&root);
        state.touch(&root);
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let responder = Responder::handle(move |request| {
            let mut state = shared.lock().expect("fake state");
            state.handle(request)
        });
        state.lock().expect("fake state").base = responder.url();
        Self { responder, state }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }

    /// The Graph endpoint, `http://127.0.0.1:<port>/v1.0/`.
    pub fn api(&self) -> String {
        format!("{}/v1.0/", self.responder.url())
    }

    pub fn drive_id(&self) -> String {
        self.lock().drive_id.clone()
    }

    pub fn drive_root(&self) -> String {
        self.lock().root.clone()
    }

    /// A configuration for this drive, mounting `root`, with the smallest
    /// fragment size so chunking is exercised cheaply.
    pub fn config(&self, root: RootSelector) -> OneDriveConfig {
        OneDriveConfig {
            api: self.api(),
            drive: crate::DriveSelector::Me,
            root,
            upload_chunk: FRAGMENT_UNIT,
        }
    }

    /// Items per `children` and `delta` page.
    pub fn set_page_size(&self, size: usize) {
        self.lock().page_size = size.max(1);
    }

    /// Create the folders of `path` below the drive root; returns the id of
    /// the last one.
    pub fn mkdirs(&self, path: &str) -> String {
        let mut state = self.lock();
        let mut folder = state.root.clone();
        for name in path.split('/').filter(|n| !n.is_empty()) {
            folder = match state.child(&folder, name) {
                Some(id) => id,
                None => state.insert(&folder, name, Kind::Folder, Vec::new()),
            };
        }
        folder
    }

    /// Create or replace a file at `path` below the drive root.
    pub fn put(&self, path: &str, bytes: &[u8]) -> String {
        let (parent, name) = split_path(path);
        let folder = self.mkdirs(parent);
        let mut state = self.lock();
        match state.child(&folder, name) {
            Some(id) => {
                state.write(&id, bytes.to_vec());
                id
            }
            None => state.insert(&folder, name, Kind::File, bytes.to_vec()),
        }
    }

    /// Create a `OneNote` notebook (a `package` item) in folder `parent`.
    pub fn put_notebook(&self, parent: &str, name: &str) -> Option<String> {
        let mut state = self.lock();
        if state.child(parent, name).is_some() || !state.items.contains_key(parent) {
            return None;
        }
        Some(state.insert(parent, name, Kind::Notebook, Vec::new()))
    }

    /// The current body of an item, if it exists.
    pub fn bytes(&self, id: &str) -> Option<Vec<u8>> {
        self.lock().items.get(id).map(|node| node.bytes.clone())
    }

    /// Names of items in the recycle bin.
    pub fn recycle_bin(&self) -> Vec<String> {
        self.lock()
            .recycle_bin
            .iter()
            .map(|(_, node)| node.name.clone())
            .collect()
    }

    pub fn inject(&self, fault: FakeFault) {
        self.lock().faults.push_back(fault);
    }

    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    /// Forget every delta token issued so far.
    pub fn reset_cursors(&self) {
        let mut state = self.lock();
        state.epoch = state.epoch.saturating_add(1);
    }

    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// Open upload sessions.
    pub fn open_uploads(&self) -> usize {
        self.lock().uploads.len()
    }
}

fn split_path(path: &str) -> (&str, &str) {
    let path = path.trim_matches('/');
    match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    }
}

fn guid_for(id: &str) -> String {
    let digest = sha2::Sha256::digest(id.as_bytes());
    let hex = hex::encode_upper(digest);
    let part = |range: std::ops::Range<usize>| hex.get(range).unwrap_or("0").to_owned();
    format!(
        "{}-{}-{}-{}-{}",
        part(0..8),
        part(8..12),
        part(12..16),
        part(16..20),
        part(20..32)
    )
}

fn in_an_hour() -> SystemTime {
    now()
        .checked_add(Duration::from_secs(3600))
        .unwrap_or_else(now)
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn json_reply(status: u16, value: &Value) -> Reply {
    Reply::status(status)
        .header("Content-Type", "application/json")
        .body(value.to_string())
}

fn error(status: u16, code: &str) -> Reply {
    json_reply(
        status,
        &json!({"error": {"code": code, "message": format!("fake graph: {code}")}}),
    )
}

/// Characters no `OneDrive` flavour accepts in a name.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains(['"', '*', ':', '<', '>', '?', '/', '\\', '|'])
        && !name.ends_with(['.', ' '])
        && !name.starts_with(' ')
}

/// What a request's `If-Match` asks for.
fn precondition_holds(request: &Request, node: &Node) -> bool {
    match request.header("if-match") {
        None | Some("*") => true,
        Some(value) => value == node.etag() || value == node.ctag(),
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn body_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap_or(Value::Null)
}

/// A Graph request path after `drives/{id}/`, split at the `:/path:`
/// syntax: `(item, path below it, rest)`.
struct Address {
    item: String,
    path: Option<Vec<String>>,
    rest: Vec<String>,
}

fn parse_address(raw: &str) -> Option<Address> {
    // `raw` is still percent-encoded, so a literal ':' is syntax.
    let (item_part, tail) = match raw.split_once(":/") {
        Some((item, tail)) => (item, Some(tail)),
        None => (raw, None),
    };
    let mut item_segments = item_part.split('/');
    let first = item_segments.next()?;
    let (item, mut rest): (String, Vec<String>) = match first {
        "root" => ("root".into(), Vec::new()),
        "items" => (percent_decode(item_segments.next()?), Vec::new()),
        _ => return None,
    };
    rest.extend(item_segments.map(percent_decode));
    let path = match tail {
        None => None,
        Some(tail) => {
            let (path, after) = tail.split_once(':').unwrap_or((tail, ""));
            rest = after
                .split('/')
                .filter(|s| !s.is_empty())
                .map(percent_decode)
                .collect();
            Some(
                path.split('/')
                    .filter(|s| !s.is_empty())
                    .map(percent_decode)
                    .collect(),
            )
        }
    };
    Some(Address { item, path, rest })
}

impl State {
    fn new_id(&mut self) -> String {
        self.next_id = self.next_id.saturating_add(1);
        match self.flavor {
            DriveType::Personal => {
                format!("{}!{}", self.drive_id.to_ascii_uppercase(), self.next_id)
            }
            DriveType::Business | DriveType::DocumentLibrary => {
                format!("01FAKEGRAPH{:024}", self.next_id)
            }
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    /// Record a change of `id` and of its parent chain, as delta reports
    /// the hierarchy above every change.
    fn touch(&mut self, id: &str) {
        let mut current = Some(id.to_owned());
        while let Some(id) = current {
            let seq = self.next_seq();
            current = match self.items.get_mut(&id) {
                Some(node) => {
                    node.seq = seq;
                    node.parent.clone()
                }
                None => None,
            };
        }
    }

    fn child(&self, parent: &str, name: &str) -> Option<String> {
        let wanted = name.to_lowercase();
        self.items
            .iter()
            .find(|(_, node)| {
                node.parent.as_deref() == Some(parent) && node.name.to_lowercase() == wanted
            })
            .map(|(id, _)| id.clone())
    }

    fn children(&self, parent: &str) -> Vec<String> {
        let mut out: Vec<(String, String)> = self
            .items
            .iter()
            .filter(|(_, node)| node.parent.as_deref() == Some(parent))
            .map(|(id, node)| (node.name.to_lowercase(), id.clone()))
            .collect();
        out.sort();
        out.into_iter().map(|(_, id)| id).collect()
    }

    fn descendants(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![id.to_owned()];
        while let Some(next) = pending.pop() {
            for child in self.children(&next) {
                pending.push(child.clone());
                out.push(child);
            }
        }
        out
    }

    fn resolve(&self, address: &Address) -> Option<String> {
        let mut current = if address.item == "root" {
            self.root.clone()
        } else {
            address.item.clone()
        };
        if !self.items.contains_key(&current) {
            return None;
        }
        for name in address.path.iter().flatten() {
            current = self.child(&current, name)?;
        }
        Some(current)
    }

    fn insert(&mut self, parent: &str, name: &str, kind: Kind, bytes: Vec<u8>) -> String {
        let id = self.new_id();
        let now = now();
        let mut node = Node {
            name: name.to_owned(),
            parent: Some(parent.to_owned()),
            kind,
            bytes: Vec::new(),
            guid: guid_for(&id),
            etag: 1,
            ctag: 1,
            created: now,
            modified: now,
            seq: 0,
            versions: Vec::new(),
            version_counter: 0,
            link: None,
        };
        if kind == Kind::File {
            self.push_version(&mut node, bytes);
        }
        self.items.insert(id.clone(), node);
        self.touch(&id);
        id
    }

    fn version_id(&self, counter: u64) -> String {
        match self.flavor {
            DriveType::Personal => base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(format!("v{counter}"))
                .to_ascii_lowercase(),
            DriveType::Business | DriveType::DocumentLibrary => format!("{counter}.0"),
        }
    }

    fn push_version(&self, node: &mut Node, bytes: Vec<u8>) {
        node.version_counter = node.version_counter.saturating_add(1);
        node.versions.push(StoredVersion {
            id: self.version_id(node.version_counter),
            bytes: bytes.clone(),
            modified: now(),
        });
        if node.versions.len() > VERSIONS_KEPT {
            node.versions.remove(0);
        }
        node.bytes = bytes;
        node.modified = now();
    }

    fn write(&mut self, id: &str, bytes: Vec<u8>) {
        let Some(mut node) = self.items.remove(id) else {
            return;
        };
        self.push_version(&mut node, bytes);
        node.etag = node.etag.saturating_add(1);
        node.ctag = node.ctag.saturating_add(1);
        self.items.insert(id.to_owned(), node);
        self.touch(id);
    }

    fn folder_size(&self, id: &str) -> u64 {
        self.descendants(id)
            .iter()
            .filter_map(|child| self.items.get(child))
            .map(|node| node.bytes.len() as u64)
            .sum()
    }

    fn path_of(&self, id: &str) -> String {
        let mut names = Vec::new();
        let mut current = self.items.get(id).and_then(|n| n.parent.as_deref());
        let mut guard = 0_u32;
        while let Some(parent) = current {
            guard = guard.saturating_add(1);
            if parent == self.root || guard > 512 {
                break;
            }
            let Some(node) = self.items.get(parent) else {
                break;
            };
            names.push(encode_segment(&node.name));
            current = node.parent.as_deref();
        }
        names.reverse();
        format!("/drive/root:/{}", names.join("/"))
    }

    fn download_url(&mut self, bytes: Vec<u8>) -> String {
        let token = format!("{}-{}", self.next_seq(), self.downloads.len());
        self.downloads.insert(token.clone(), bytes);
        format!("{}/download/{token}", self.base)
    }

    fn item_json(&mut self, id: &str, delta: bool) -> Value {
        let Some(node) = self.items.get(id).cloned() else {
            return Value::Null;
        };
        let business = self.flavor != DriveType::Personal;
        let size = if node.is_container() {
            self.folder_size(id)
        } else {
            node.bytes.len() as u64
        };
        let mut value = json!({
            "id": id,
            "name": node.name,
            "eTag": node.etag(),
            "size": size,
            "createdDateTime": crate::time::format(node.created),
            "lastModifiedDateTime": crate::time::format(node.modified),
            "webUrl": format!("https://onedrive.example/{}", encode_segment(id)),
            "createdBy": {"user": {"displayName": "Ada Lovelace"}},
        });
        if !(delta && business) {
            value["cTag"] = json!(node.ctag());
        }
        if let Some(parent) = &node.parent {
            let mut reference = json!({
                "driveId": self.drive_id,
                "driveType": if business { "business" } else { "personal" },
                "id": parent,
            });
            if !delta {
                reference["path"] = json!(self.path_of(id));
            }
            value["parentReference"] = reference;
        }
        match node.kind {
            Kind::Root => {
                value["root"] = json!({});
                value["folder"] = json!({"childCount": self.children(id).len()});
            }
            Kind::Folder => {
                value["folder"] = json!({"childCount": self.children(id).len()});
            }
            Kind::Notebook => {
                value["package"] = json!({"type": "oneNote"});
            }
            Kind::File => {
                let mut hashes = json!({"quickXorHash": quick_xor_base64(&node.bytes)});
                if !business {
                    let sha = hex::encode_upper(sha2::Sha256::digest(&node.bytes));
                    hashes["sha256Hash"] = json!(sha);
                }
                value["file"] = json!({
                    "mimeType": "application/octet-stream",
                    "hashes": hashes,
                });
                if !delta {
                    value["@microsoft.graph.downloadUrl"] = json!(self.download_url(node.bytes));
                }
            }
        }
        value
    }

    fn tombstone_json(&self, tombstone: &Tombstone) -> Value {
        let mut value = json!({
            "id": tombstone.id,
            "deleted": {"state": "deleted"},
        });
        if self.flavor == DriveType::Personal {
            value["name"] = json!(tombstone.name);
        }
        if let Some(parent) = &tombstone.parent {
            value["parentReference"] = json!({"driveId": self.drive_id, "id": parent});
        }
        if tombstone.folder {
            value["folder"] = json!({});
        } else {
            value["file"] = json!({});
        }
        value
    }

    fn drive_json(&self) -> Value {
        let used: u64 = self.items.values().map(|n| n.bytes.len() as u64).sum();
        let deleted: u64 = self
            .recycle_bin
            .iter()
            .map(|(_, n)| n.bytes.len() as u64)
            .sum();
        let mut quota = json!({
            "deleted": deleted,
            "remaining": QUOTA_TOTAL.saturating_sub(used).saturating_sub(deleted),
            "state": "normal",
            "total": QUOTA_TOTAL,
        });
        let drive_type = if self.flavor == DriveType::Personal {
            quota["used"] = json!(used);
            "personal"
        } else {
            "business"
        };
        json!({
            "id": self.drive_id,
            "driveType": drive_type,
            "owner": {"user": {"displayName": "Ada Lovelace"}},
            "quota": quota,
        })
    }

    /// Pop the first queued fault that applies to a request of this kind.
    fn take_fault(&mut self, applies: impl Fn(FakeFault) -> bool) -> Option<FakeFault> {
        let index = self.faults.iter().position(|fault| applies(*fault))?;
        self.faults.remove(index)
    }

    fn handle(&mut self, request: &Request) -> Option<Reply> {
        if self.offline {
            return None;
        }
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((request.target.as_str(), ""));
        let query = parse_query(query);
        if let Some(token) = path.strip_prefix("/download/") {
            return Some(self.download(request, token));
        }
        if let Some(session) = path.strip_prefix("/upload/") {
            return Some(self.upload(request, session));
        }
        let Some(graph) = path.strip_prefix("/v1.0/") else {
            return Some(error(404, "itemNotFound"));
        };
        self.stats.graph_requests = self.stats.graph_requests.saturating_add(1);
        match self.take_fault(|f| !matches!(f, FakeFault::TruncatedBody | FakeFault::ClosedEarly)) {
            Some(FakeFault::RateLimited) => {
                return Some(error(429, "activityLimitReached").header("Retry-After", "1"));
            }
            Some(FakeFault::ServerError) => return Some(error(503, "serviceNotAvailable")),
            Some(FakeFault::ExpiredCredential) => {
                return Some(error(401, "InvalidAuthenticationToken"));
            }
            _ => {}
        }
        if request.header("authorization") != Some(format!("Bearer {TOKEN}").as_str()) {
            return Some(error(401, "InvalidAuthenticationToken"));
        }
        Some(self.graph(request, graph, &query))
    }

    fn graph(&mut self, request: &Request, path: &str, query: &HashMap<String, String>) -> Reply {
        let method = request.method.as_str();
        if path == "me/drive" {
            return json_reply(200, &self.drive_json());
        }
        let Some(rest) = path.strip_prefix("drives/") else {
            return error(400, "invalidRequest");
        };
        let (drive, rest) = rest.split_once('/').unwrap_or((rest, ""));
        if percent_decode(drive) != self.drive_id {
            return error(404, "itemNotFound");
        }
        if rest.is_empty() {
            return json_reply(200, &self.drive_json());
        }
        if rest == "root/delta" {
            return self.delta(query);
        }
        let Some(address) = parse_address(rest) else {
            return error(400, "invalidRequest");
        };
        let rest: Vec<&str> = address.rest.iter().map(String::as_str).collect();
        match (method, rest.as_slice()) {
            ("PUT", ["content"]) => self.simple_put(request, &address, query),
            ("POST", ["createUploadSession"]) => self.create_session(request, &address),
            _ => {
                let Some(id) = self.resolve(&address) else {
                    return error(404, "itemNotFound");
                };
                match (method, rest.as_slice()) {
                    ("GET", []) => {
                        let value = self.item_json(&id, false);
                        json_reply(200, &value)
                    }
                    ("PATCH", []) => self.patch(request, &id),
                    ("DELETE", []) => self.delete(request, &id),
                    ("GET", ["children"]) => self.list(&id, query, request),
                    ("POST", ["children"]) => self.create_folder(request, &id),
                    ("GET", ["content"]) => self.content(&id),
                    ("GET", ["versions"]) => self.versions(&id),
                    ("GET", ["versions", version, "content"]) => self.version_content(&id, version),
                    ("POST", ["versions", version, "restoreVersion"]) => self.restore(&id, version),
                    ("POST", ["createLink"]) => self.create_link(request, &id),
                    _ => error(400, "invalidRequest"),
                }
            }
        }
    }

    fn list(&mut self, id: &str, query: &HashMap<String, String>, request: &Request) -> Reply {
        let skip: usize = query
            .get("$skiptoken")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let children = self.children(id);
        let end = skip.saturating_add(self.page_size).min(children.len());
        let page: Vec<Value> = children
            .get(skip..end)
            .unwrap_or_default()
            .to_vec()
            .iter()
            .map(|child| self.item_json(child, false))
            .collect();
        let mut value = json!({"value": page});
        if end < children.len() {
            let path = request.target.split('?').next().unwrap_or_default();
            value["@odata.nextLink"] = json!(format!("{}{path}?$skiptoken={end}", self.base));
        }
        json_reply(200, &value)
    }

    fn delta(&mut self, query: &HashMap<String, String>) -> Reply {
        self.stats.delta_requests = self.stats.delta_requests.saturating_add(1);
        let fresh = format!(
            "{}/v1.0/drives/{}/root/delta",
            self.base,
            encode_segment(&self.drive_id)
        );
        // Tokens: `E_U` (a deltaLink: changes after U) or `E_L_U_F` (a
        // nextLink: the round up to U, continuing after L; F=1 while the
        // first full enumeration runs).
        let (after, upto, full) = match query.get("token") {
            None => (0, self.seq, true),
            Some(token) => {
                let parts: Vec<u64> = token.split('_').filter_map(|p| p.parse().ok()).collect();
                match parts.as_slice() {
                    [epoch, upto] if *epoch == self.epoch => (*upto, self.seq, false),
                    [epoch, after, upto, full] if *epoch == self.epoch => {
                        (*after, *upto, *full == 1)
                    }
                    _ => {
                        return error(410, "resyncChangesApplyDifferences")
                            .header("Location", &fresh);
                    }
                }
            }
        };
        let mut entries: Vec<(u64, Option<String>, Option<Tombstone>)> = self
            .items
            .iter()
            .filter(|(_, node)| node.seq > after && node.seq <= upto)
            .map(|(id, node)| (node.seq, Some(id.clone()), None))
            .collect();
        if !full {
            entries.extend(
                self.tombstones
                    .iter()
                    .filter(|t| t.seq > after && t.seq <= upto)
                    .map(|t| (t.seq, None, Some(t.clone()))),
            );
        }
        entries.sort_by_key(|(seq, _, _)| *seq);
        let more = entries.len() > self.page_size;
        entries.truncate(self.page_size);
        let last = entries.last().map_or(upto, |(seq, _, _)| *seq);
        let value: Vec<Value> = entries
            .iter()
            .map(|(_, id, tombstone)| match (id, tombstone) {
                (Some(id), _) => self.item_json(id, true),
                (None, Some(tombstone)) => self.tombstone_json(tombstone),
                (None, None) => Value::Null,
            })
            .collect();
        let mut body = json!({"value": value});
        if more {
            body["@odata.nextLink"] = json!(format!(
                "{fresh}?token={}_{last}_{upto}_{}",
                self.epoch,
                u8::from(full)
            ));
        } else {
            body["@odata.deltaLink"] = json!(format!("{fresh}?token={}_{upto}", self.epoch));
        }
        json_reply(200, &body)
    }

    fn create_folder(&mut self, request: &Request, parent: &str) -> Reply {
        if !self.items.get(parent).is_some_and(Node::is_container) {
            return error(400, "invalidRequest");
        }
        let body = body_json(request);
        let Some(name) = body["name"].as_str().map(str::to_owned) else {
            return error(400, "invalidRequest");
        };
        if body.get("folder").is_none() {
            return error(400, "invalidRequest");
        }
        let behavior = body["@microsoft.graph.conflictBehavior"]
            .as_str()
            .unwrap_or("fail");
        let name = match self.place(parent, &name, behavior) {
            Ok(name) => name,
            Err(reply) => return reply,
        };
        let id = self.insert(parent, &name, Kind::Folder, Vec::new());
        let value = self.item_json(&id, false);
        json_reply(201, &value)
    }

    /// The name a new item gets under `conflictBehavior`, or the refusal.
    /// `replace` removes the existing item.
    fn place(&mut self, parent: &str, name: &str, behavior: &str) -> Result<String, Reply> {
        if !valid_name(name) {
            return Err(error(400, "invalidRequest"));
        }
        let Some(existing) = self.child(parent, name) else {
            return Ok(name.to_owned());
        };
        match behavior {
            "replace" => {
                self.remove(&existing);
                Ok(name.to_owned())
            }
            "rename" => {
                let (stem, extension) = match name.rfind('.') {
                    Some(dot) if dot > 0 => name.split_at(dot),
                    _ => (name, ""),
                };
                let mut counter = 1_u32;
                loop {
                    let candidate = format!("{stem} {counter}{extension}");
                    if self.child(parent, &candidate).is_none() {
                        return Ok(candidate);
                    }
                    counter = counter.saturating_add(1);
                }
            }
            _ => Err(error(409, "nameAlreadyExists")),
        }
    }

    fn patch(&mut self, request: &Request, id: &str) -> Reply {
        let Some(node) = self.items.get(id).cloned() else {
            return error(404, "itemNotFound");
        };
        if node.kind == Kind::Root {
            return error(403, "accessDenied");
        }
        if !precondition_holds(request, &node) {
            return error(412, "preconditionFailed");
        }
        let body = body_json(request);
        let name = body["name"].as_str().unwrap_or(&node.name).to_owned();
        let parent = body["parentReference"]["id"]
            .as_str()
            .map_or_else(|| node.parent.clone().unwrap_or_default(), str::to_owned);
        if !valid_name(&name) {
            return error(400, "invalidRequest");
        }
        if !self.items.get(&parent).is_some_and(Node::is_container) {
            return error(400, "invalidRequest");
        }
        if parent == id || self.descendants(id).contains(&parent) {
            return error(400, "invalidRequest");
        }
        if self.child(&parent, &name).is_some_and(|other| other != id) {
            return error(409, "nameAlreadyExists");
        }
        let old_parent = node.parent;
        if let Some(node) = self.items.get_mut(id) {
            node.name = name;
            node.parent = Some(parent);
            node.etag = node.etag.saturating_add(1);
            node.modified = now();
        }
        if let Some(old) = old_parent {
            self.touch(&old);
        }
        self.touch(id);
        let value = self.item_json(id, false);
        json_reply(200, &value)
    }

    /// Move an item and everything below it to the recycle bin.
    fn remove(&mut self, id: &str) {
        let mut doomed = self.descendants(id);
        doomed.push(id.to_owned());
        let parent = self.items.get(id).and_then(|n| n.parent.clone());
        for victim in doomed {
            let Some(node) = self.items.remove(&victim) else {
                continue;
            };
            let seq = self.next_seq();
            self.tombstones.push(Tombstone {
                id: victim.clone(),
                parent: node.parent.clone(),
                name: node.name.clone(),
                folder: node.is_container(),
                seq,
            });
            self.recycle_bin.push((victim, node));
        }
        if let Some(parent) = parent {
            self.touch(&parent);
        }
    }

    fn delete(&mut self, request: &Request, id: &str) -> Reply {
        let Some(node) = self.items.get(id) else {
            return error(404, "itemNotFound");
        };
        if node.kind == Kind::Root {
            return error(403, "accessDenied");
        }
        if !precondition_holds(request, node) {
            return error(412, "preconditionFailed");
        }
        self.remove(id);
        Reply::status(204)
    }

    fn content(&mut self, id: &str) -> Reply {
        let Some(node) = self.items.get(id) else {
            return error(404, "itemNotFound");
        };
        if node.kind != Kind::File {
            return error(400, "invalidRequest");
        }
        let bytes = node.bytes.clone();
        let url = self.download_url(bytes);
        Reply::status(302).header("Location", &url)
    }

    fn versions(&self, id: &str) -> Reply {
        let Some(node) = self.items.get(id) else {
            return error(404, "itemNotFound");
        };
        if node.kind != Kind::File {
            return error(400, "invalidRequest");
        }
        let value: Vec<Value> = node
            .versions
            .iter()
            .rev()
            .map(|version| {
                json!({
                    "id": version.id,
                    "lastModifiedDateTime": crate::time::format(version.modified),
                    "size": version.bytes.len(),
                    "lastModifiedBy": {"user": {"displayName": "Ada Lovelace"}},
                })
            })
            .collect();
        json_reply(200, &json!({"value": value}))
    }

    fn version_bytes(&self, id: &str, version: &str) -> Option<Vec<u8>> {
        self.items
            .get(id)?
            .versions
            .iter()
            .find(|v| v.id == version)
            .map(|v| v.bytes.clone())
    }

    fn version_content(&mut self, id: &str, version: &str) -> Reply {
        match self.version_bytes(id, version) {
            Some(bytes) => {
                let url = self.download_url(bytes);
                Reply::status(302).header("Location", &url)
            }
            None => error(404, "itemNotFound"),
        }
    }

    fn restore(&mut self, id: &str, version: &str) -> Reply {
        match self.version_bytes(id, version) {
            Some(bytes) => {
                self.write(id, bytes);
                Reply::status(204)
            }
            None => error(404, "itemNotFound"),
        }
    }

    fn create_link(&mut self, request: &Request, id: &str) -> Reply {
        let body = body_json(request);
        let kind = body["type"].as_str().unwrap_or("view").to_owned();
        let business = self.flavor != DriveType::Personal;
        let scope = body["scope"]
            .as_str()
            .unwrap_or(if business {
                "organization"
            } else {
                "anonymous"
            })
            .to_owned();
        if scope == "organization" && !business {
            return error(400, "invalidRequest");
        }
        let Some(node) = self.items.get_mut(id) else {
            return error(404, "itemNotFound");
        };
        let (status, url) = if let Some(url) = &node.link {
            (200, url.clone())
        } else {
            let url = format!("https://1drv.ms/u/s!{}", encode_segment(id));
            node.link = Some(url.clone());
            (201, url)
        };
        json_reply(
            status,
            &json!({
                "id": format!("link-{id}"),
                "roles": [if kind == "edit" { "write" } else { "read" }],
                "link": {"type": kind, "scope": scope, "webUrl": url},
            }),
        )
    }

    /// `PUT …/content`: replace by id, or create/replace by path.
    fn simple_put(
        &mut self,
        request: &Request,
        address: &Address,
        query: &HashMap<String, String>,
    ) -> Reply {
        if let Some(id) = self.resolve(address) {
            let Some(node) = self.items.get(&id) else {
                return error(404, "itemNotFound");
            };
            if node.kind != Kind::File {
                return error(409, "nameAlreadyExists");
            }
            if !precondition_holds(request, node) {
                return error(412, "preconditionFailed");
            }
            let behavior = query
                .get("@microsoft.graph.conflictBehavior")
                .map_or("replace", String::as_str);
            if address.path.is_some() && behavior == "fail" {
                return error(409, "nameAlreadyExists");
            }
            self.write(&id, request.body.clone());
            let value = self.item_json(&id, false);
            return json_reply(200, &value);
        }
        let Some((parent, name)) = self.creation_target(address) else {
            return error(404, "itemNotFound");
        };
        if !valid_name(&name) {
            return error(400, "invalidRequest");
        }
        let id = self.insert(&parent, &name, Kind::File, request.body.clone());
        let value = self.item_json(&id, false);
        json_reply(201, &value)
    }

    /// For a path address naming a missing item: its parent folder and name.
    fn creation_target(&self, address: &Address) -> Option<(String, String)> {
        let path = address.path.as_ref()?;
        let (name, parents) = path.split_last()?;
        let parent = self.resolve(&Address {
            item: address.item.clone(),
            path: Some(parents.to_vec()),
            rest: Vec::new(),
        })?;
        self.items
            .get(&parent)
            .is_some_and(Node::is_container)
            .then(|| (parent, name.clone()))
    }

    fn create_session(&mut self, request: &Request, address: &Address) -> Reply {
        let body = body_json(request);
        let behavior = body["item"]["@microsoft.graph.conflictBehavior"]
            .as_str()
            .unwrap_or("fail")
            .to_owned();
        let target = match (&address.path, self.resolve(address)) {
            // By id: replace that file.
            (None, Some(id)) => {
                let Some(node) = self.items.get(&id) else {
                    return error(404, "itemNotFound");
                };
                if node.kind != Kind::File {
                    return error(400, "invalidRequest");
                }
                if !precondition_holds(request, node) {
                    return error(412, "preconditionFailed");
                }
                SessionTarget::Replace(id)
            }
            (None, None) => return error(404, "itemNotFound"),
            // By path: a name clash is only detected at commit.
            (Some(_), _) => {
                let Some(path) = &address.path else {
                    return error(400, "invalidRequest");
                };
                let Some((name, parents)) = path.split_last() else {
                    return error(400, "invalidRequest");
                };
                let Some(parent) = self.resolve(&Address {
                    item: address.item.clone(),
                    path: Some(parents.to_vec()),
                    rest: Vec::new(),
                }) else {
                    return error(404, "itemNotFound");
                };
                if !valid_name(name) {
                    return error(400, "invalidRequest");
                }
                if let Some(given) = body["item"]["name"].as_str()
                    && given != name
                {
                    return error(400, "invalidRequest");
                }
                SessionTarget::Create {
                    parent,
                    name: name.clone(),
                }
            }
        };
        let session = format!("s{}", self.next_seq());
        self.uploads.insert(
            session.clone(),
            UploadSession {
                target,
                behavior,
                size: 0,
                received: Vec::new(),
            },
        );
        json_reply(
            200,
            &json!({
                "uploadUrl": format!("{}/upload/{session}", self.base),
                "expirationDateTime": crate::time::format(in_an_hour()),
                "nextExpectedRanges": ["0-"],
            }),
        )
    }

    fn preauthenticated(&mut self, request: &Request) -> Option<Reply> {
        if request.header("authorization").is_some() {
            self.stats.bearer_leaks = self.stats.bearer_leaks.saturating_add(1);
            return Some(error(401, "unauthenticated"));
        }
        None
    }

    fn upload(&mut self, request: &Request, session_id: &str) -> Reply {
        if let Some(reply) = self.preauthenticated(request) {
            return reply;
        }
        match self.take_fault(|f| matches!(f, FakeFault::RateLimited | FakeFault::ServerError)) {
            Some(FakeFault::RateLimited) => {
                return error(429, "activityLimitReached").header("Retry-After", "1");
            }
            Some(_) => return error(503, "serviceNotAvailable"),
            None => {}
        }
        let Some(session) = self.uploads.get(session_id).cloned() else {
            return error(404, "itemNotFound");
        };
        let expiry = crate::time::format(in_an_hour());
        match request.method.as_str() {
            "GET" => json_reply(
                200,
                &json!({
                    "expirationDateTime": expiry,
                    "nextExpectedRanges": [format!("{}-", session.received.len())],
                }),
            ),
            "DELETE" => {
                self.uploads.remove(session_id);
                Reply::status(204)
            }
            "PUT" => self.fragment(request, session_id, session, &expiry),
            _ => error(405, "invalidRequest"),
        }
    }

    fn fragment(
        &mut self,
        request: &Request,
        session_id: &str,
        mut session: UploadSession,
        expiry: &str,
    ) -> Reply {
        self.stats.fragments = self.stats.fragments.saturating_add(1);
        let Some((range, Some(total))) = request
            .header("content-range")
            .and_then(cloud_source::http::parse_content_range)
        else {
            return error(400, "invalidRequest");
        };
        if session.size != 0 && session.size != total {
            return error(400, "invalidRequest");
        }
        session.size = total;
        let len = range.end.saturating_sub(range.start);
        if request.body.len() as u64 != len || range.end > total {
            return error(400, "invalidRequest");
        }
        if range.start != session.received.len() as u64 {
            return error(416, "invalidRange");
        }
        if range.end < total && len % FRAGMENT_UNIT != 0 {
            return error(400, "invalidRequest");
        }
        session.received.extend_from_slice(&request.body);
        if range.end < total {
            self.uploads.insert(session_id.to_owned(), session);
            return json_reply(
                202,
                &json!({
                    "expirationDateTime": expiry,
                    "nextExpectedRanges": [format!("{}-", range.end)],
                }),
            );
        }
        // The last byte commits the file.
        let (status, id) = match &session.target {
            SessionTarget::Replace(id) => {
                if !self.items.contains_key(id) {
                    self.uploads.insert(session_id.to_owned(), session);
                    return error(404, "itemNotFound");
                }
                let id = id.clone();
                self.write(&id, session.received.clone());
                (200, id)
            }
            SessionTarget::Create { parent, name } => {
                let (parent, name) = (parent.clone(), name.clone());
                let name = match self.place(&parent, &name, &session.behavior) {
                    Ok(name) => name,
                    Err(reply) => {
                        self.uploads.insert(session_id.to_owned(), session);
                        return reply;
                    }
                };
                let id = self.insert(&parent, &name, Kind::File, session.received.clone());
                (201, id)
            }
        };
        self.uploads.remove(session_id);
        let value = self.item_json(&id, false);
        json_reply(status, &value)
    }

    fn download(&mut self, request: &Request, token: &str) -> Reply {
        if let Some(reply) = self.preauthenticated(request) {
            return reply;
        }
        let fault = self.take_fault(|f| {
            matches!(
                f,
                FakeFault::RateLimited
                    | FakeFault::ServerError
                    | FakeFault::TruncatedBody
                    | FakeFault::ClosedEarly
            )
        });
        match fault {
            Some(FakeFault::RateLimited) => {
                return error(429, "activityLimitReached").header("Retry-After", "1");
            }
            Some(FakeFault::ServerError) => return error(503, "serviceNotAvailable"),
            _ => {}
        }
        let Some(bytes) = self.downloads.get(token).cloned() else {
            return error(404, "itemNotFound");
        };
        self.stats.downloads = self.stats.downloads.saturating_add(1);
        let total = bytes.len() as u64;
        let (status, body, content_range) = match request.header("range") {
            None => (200, bytes, None),
            Some(range) => {
                self.stats.ranged_downloads = self.stats.ranged_downloads.saturating_add(1);
                let Some(wanted) = parse_range(range, total) else {
                    return Reply::status(416).header("Content-Range", &format!("bytes */{total}"));
                };
                let start = usize::try_from(wanted.start).unwrap_or(usize::MAX);
                let end = usize::try_from(wanted.end).unwrap_or(usize::MAX);
                (
                    206,
                    bytes.get(start..end).unwrap_or_default().to_vec(),
                    Some(format!(
                        "bytes {}-{}/{total}",
                        wanted.start,
                        wanted.end.saturating_sub(1)
                    )),
                )
            }
        };
        self.stats.bytes_served = self.stats.bytes_served.saturating_add(body.len() as u64);
        let mut reply = Reply::status(status).header("Content-Type", "application/octet-stream");
        if let Some(content_range) = content_range {
            reply = reply.header("Content-Range", &content_range);
        }
        match fault {
            Some(FakeFault::TruncatedBody) => reply.body(body).cut_short(true),
            Some(FakeFault::ClosedEarly) => reply.body(body).cut_short(false),
            _ => reply.body(body),
        }
    }
}

/// `Range: bytes=a-b`, `bytes=a-` or `bytes=-n` against `total`.
fn parse_range(value: &str, total: u64) -> Option<std::ops::Range<u64>> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (first, last) = spec.split_once('-')?;
    let range = if first.is_empty() {
        let suffix: u64 = last.parse().ok()?;
        total.saturating_sub(suffix)..total
    } else {
        let first: u64 = first.parse().ok()?;
        let end = if last.is_empty() {
            total
        } else {
            last.parse::<u64>().ok()?.saturating_add(1).min(total)
        };
        first..end
    };
    (range.start < range.end).then_some(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split_items_paths_and_actions() {
        let address = parse_address("items/A%21100:/Docs/a%20b.txt:/createUploadSession").unwrap();
        assert_eq!(address.item, "A!100");
        assert_eq!(address.path.unwrap(), ["Docs", "a b.txt"]);
        assert_eq!(address.rest, ["createUploadSession"]);
        let plain = parse_address("items/X/versions/1.0/content").unwrap();
        assert!(plain.path.is_none());
        assert_eq!(plain.rest, ["versions", "1.0", "content"]);
        let root = parse_address("root:/a.txt:").unwrap();
        assert_eq!(root.item, "root");
        assert!(root.rest.is_empty());
    }

    #[test]
    fn ranges_parse_like_http() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(0..10));
        assert_eq!(parse_range("bytes=90-", 100), Some(90..100));
        assert_eq!(parse_range("bytes=-5", 100), Some(95..100));
        assert_eq!(parse_range("bytes=100-", 100), None);
    }
}
