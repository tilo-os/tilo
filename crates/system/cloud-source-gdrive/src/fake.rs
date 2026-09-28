//! An in-memory Google Drive (API v3) on the shared test responder.
//!
//! It keeps Drive's semantics where a client can trip over them:
//!
//! - items are ids with a `parents` list; names are not unique in a folder,
//!   and legacy items can have several parents ([`FakeDrive::add_parent`];
//!   the API itself refuses to add a second parent, as Drive does now);
//! - `fields` selects what a response contains, with Drive's defaults
//!   (`id,name,mimeType`) when absent, so a client that forgets to ask for a
//!   field does not get it;
//! - `int64` values are JSON strings;
//! - `q` understands the subset of the search grammar clients send
//!   (`'<id>' in parents`, `trashed`, `name`, `mimeType`, `and`/`or`/`not`,
//!   parentheses) and rejects the rest;
//! - Docs/Sheets/Slides/Drawings have no media (`fileNotDownloadable`) and
//!   export to their formats;
//! - the changes feed is account-wide, reports each changed file once with
//!   its latest state, and reports permanent deletions as `removed`;
//!   trashing a folder trashes its contents implicitly;
//! - resumable uploads follow the documented protocol: a session URI from
//!   `Location`, `Content-Range` chunks, `308` with the committed `Range`,
//!   non-final chunks committed only in multiples of 256 KiB, status queries
//!   and cancellation (`499`);
//! - each committed upload is a revision with MD5/SHA-1/SHA-256; unpinned
//!   revisions beyond 100 are pruned; `keepForever` pins;
//! - shared drives ([`FakeDrive::add_shared_drive`]) need
//!   `supportsAllDrives`/`includeItemsFromAllDrives`/`corpora=drive`.
//!
//! Two tokens are accepted: [`SOURCE_TOKEN`] for the client under test and
//! [`REMOTE_TOKEN`] for "another device". Faults and going offline affect
//! requests with the source token only.
//!
//! Test support only (feature `fake-server`).

#![allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::single_match_else,
    clippy::manual_let_else,
    clippy::assigning_clones,
    reason = "a test fixture: a poisoned lock has already failed the test, and sizes are bounded by the test"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use md5::Digest as _;
use serde_json::{Map, Value, json};
use test_http::{Reply, Request, Responder};

use crate::api::{self, FOLDER_MIME};
use crate::{DriveConfig, UPLOAD_QUANTUM, Url};

/// The access token of the client under test.
pub const SOURCE_TOKEN: &str = "ya29.fake-source-token";
/// The access token of the independent "other device".
pub const REMOTE_TOKEN: &str = "ya29.fake-remote-token";

/// Unpinned revisions a file keeps, as on Drive.
const MAX_REVISIONS: usize = 100;
const GOOGLE_APPS: &str = "application/vnd.google-apps.";

/// A fault for the source's next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFault {
    /// `403 userRateLimitExceeded`, the way Drive usually rate limits.
    UserRateLimited,
    /// `429 rateLimitExceeded` with `Retry-After: 1`.
    TooManyRequests,
    /// `503 backendError`.
    ServerError,
    /// The next media download declares its length but sends half.
    TruncatedBody,
    /// `401 authError` once.
    ExpiredCredential,
}

/// Request counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub file_lists: usize,
    pub change_lists: usize,
    pub media_gets: usize,
    pub upload_puts: usize,
}

#[derive(Clone, Debug)]
struct Rev {
    id: String,
    bytes: Vec<u8>,
    modified: SystemTime,
    keep_forever: bool,
}

#[derive(Clone, Debug)]
struct Node {
    id: String,
    name: String,
    mime: String,
    parents: Vec<String>,
    trashed: bool,
    explicitly_trashed: bool,
    version: u64,
    modified: SystemTime,
    revisions: Vec<Rev>,
    drive_id: Option<String>,
    permissions: Vec<Value>,
}

impl Node {
    fn is_folder(&self) -> bool {
        self.mime == FOLDER_MIME
    }

    fn is_native(&self) -> bool {
        !self.is_folder() && self.mime.starts_with(GOOGLE_APPS)
    }

    fn is_blob(&self) -> bool {
        !self.mime.starts_with(GOOGLE_APPS)
    }

    fn head(&self) -> Option<&Rev> {
        self.revisions.last()
    }
}

#[derive(Debug)]
struct Session {
    /// The file a `PATCH` session replaces.
    target: Option<String>,
    metadata: Value,
    total: Option<u64>,
    data: Vec<u8>,
    fields: Option<String>,
    /// The file id once committed.
    completed: Option<String>,
}

#[derive(Default)]
struct State {
    files: BTreeMap<String, Node>,
    root: String,
    counter: u64,
    version: u64,
    /// The next change sequence number; also the start page token.
    seq: u64,
    log: Vec<(u64, String)>,
    /// Tokens below this are forgotten.
    floor: u64,
    sessions: BTreeMap<String, Session>,
    faults: VecDeque<FakeFault>,
    truncate: bool,
    offline: bool,
    /// Bytes another client commits to a file just before the next upload
    /// to it commits, to exercise the check-then-commit window.
    race: Option<(String, Vec<u8>)>,
    stats: Stats,
}

/// A running fake Drive. Dropping it stops the server.
pub struct FakeDrive {
    responder: Responder,
    state: Arc<Mutex<State>>,
}

impl Default for FakeDrive {
    fn default() -> Self {
        Self::start()
    }
}

impl FakeDrive {
    /// An empty My Drive.
    pub fn start() -> Self {
        let mut state = State::default();
        let root = "0AfakeRootUk9PVA".to_owned();
        state.root.clone_from(&root);
        let now = SystemTime::now();
        state.files.insert(
            root.clone(),
            Node {
                id: root,
                name: "My Drive".into(),
                mime: FOLDER_MIME.into(),
                parents: Vec::new(),
                trashed: false,
                explicitly_trashed: false,
                version: 1,
                modified: now,
                revisions: Vec::new(),
                drive_id: None,
                permissions: Vec::new(),
            },
        );
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let responder = Responder::handle(move |request| handle(&shared, request));
        Self { responder, state }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake drive state")
    }

    /// The API origin, for [`DriveConfig::with_api`].
    pub fn url(&self) -> String {
        format!("{}/", self.responder.url())
    }

    /// My Drive through this server, with one-quantum upload chunks so
    /// multi-chunk uploads stay small.
    pub fn config(&self) -> DriveConfig {
        DriveConfig {
            upload_chunk: UPLOAD_QUANTUM,
            ..DriveConfig::my_drive().with_api(self.url())
        }
    }

    /// The real id behind the `root` alias.
    pub fn root_id(&self) -> String {
        self.lock().root.clone()
    }

    pub fn inject(&self, fault: FakeFault) {
        let mut state = self.lock();
        if fault == FakeFault::TruncatedBody {
            state.truncate = true;
        } else {
            state.faults.push_back(fault);
        }
    }

    /// Drop the source's connections unanswered.
    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    /// Forget every change page token issued so far.
    pub fn reset_cursors(&self) {
        let mut state = self.lock();
        // Skip a sequence number so no token handed out so far stays valid.
        state.seq += 1;
        state.floor = state.seq;
    }

    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// Create a Google-native document directly.
    pub fn put_native(&self, parent: &str, name: &str, mime: &str) -> String {
        let mut state = self.lock();
        let parent = state.resolve(parent);
        state.create_node(name, mime, vec![parent], None)
    }

    /// Create a file directly, even beside one of the same name.
    pub fn put_file(&self, parent: &str, name: &str, bytes: &[u8]) -> String {
        let mut state = self.lock();
        let parent = state.resolve(parent);
        state.create_node(
            name,
            "application/octet-stream",
            vec![parent],
            Some(bytes.to_vec()),
        )
    }

    /// Give an item a further parent, as legacy Drive items can have.
    pub fn add_parent(&self, id: &str, parent: &str) {
        let mut state = self.lock();
        let parent = state.resolve(parent);
        let version = state.bump();
        let node = state.files.get_mut(id).expect("known item");
        node.parents.push(parent);
        node.version = version;
        state.touch(id);
    }

    /// Change a file's bytes directly (another client's upload).
    pub fn write_file(&self, id: &str, bytes: &[u8]) {
        let mut state = self.lock();
        state.add_revision(id, bytes.to_vec());
    }

    /// Commit `bytes` to `id` from another client right before the next
    /// upload to `id` commits.
    pub fn race_next_commit(&self, id: &str, bytes: &[u8]) {
        self.lock().race = Some((id.to_owned(), bytes.to_vec()));
    }

    /// A new shared drive; returns its id (also its root folder's id).
    pub fn add_shared_drive(&self, name: &str) -> String {
        let mut state = self.lock();
        let id = format!("0AdrIvE{:08}", state.next());
        let version = state.bump();
        state.files.insert(
            id.clone(),
            Node {
                id: id.clone(),
                name: name.to_owned(),
                mime: FOLDER_MIME.into(),
                parents: Vec::new(),
                trashed: false,
                explicitly_trashed: false,
                version,
                modified: SystemTime::now(),
                revisions: Vec::new(),
                drive_id: Some(id.clone()),
                permissions: Vec::new(),
            },
        );
        id
    }

    /// `(revision id, keepForever)` of a file, oldest first.
    pub fn revisions(&self, id: &str) -> Vec<(String, bool)> {
        self.lock()
            .files
            .get(id)
            .map(|node| {
                node.revisions
                    .iter()
                    .map(|r| (r.id.clone(), r.keep_forever))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Current bytes of a file.
    pub fn bytes(&self, id: &str) -> Option<Vec<u8>> {
        self.lock()
            .files
            .get(id)
            .and_then(|n| n.head().map(|r| r.bytes.clone()))
    }

    /// Whether an item exists and is trashed.
    pub fn is_trashed(&self, id: &str) -> Option<bool> {
        self.lock().files.get(id).map(|n| n.trashed)
    }

    /// Permissions granted on an item.
    pub fn permissions(&self, id: &str) -> Vec<Value> {
        self.lock()
            .files
            .get(id)
            .map(|n| n.permissions.clone())
            .unwrap_or_default()
    }
}

// --- state ---------------------------------------------------------------

impl State {
    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn bump(&mut self) -> u64 {
        self.version += 1;
        self.version
    }

    fn resolve(&self, id: &str) -> String {
        if id == "root" {
            self.root.clone()
        } else {
            id.to_owned()
        }
    }

    fn touch(&mut self, id: &str) {
        let seq = self.seq;
        self.seq += 1;
        self.log.push((seq, id.to_owned()));
        if let Some(node) = self.files.get_mut(id) {
            node.modified = SystemTime::now();
        }
    }

    fn drive_of(&self, parents: &[String]) -> Option<String> {
        parents
            .iter()
            .find_map(|p| self.files.get(p).and_then(|n| n.drive_id.clone()))
    }

    fn create_node(
        &mut self,
        name: &str,
        mime: &str,
        parents: Vec<String>,
        bytes: Option<Vec<u8>>,
    ) -> String {
        let id = format!("1fake{:08}Xq", self.next());
        let version = self.bump();
        let drive_id = self.drive_of(&parents);
        self.files.insert(
            id.clone(),
            Node {
                id: id.clone(),
                name: name.to_owned(),
                mime: mime.to_owned(),
                parents,
                trashed: false,
                explicitly_trashed: false,
                version,
                modified: SystemTime::now(),
                revisions: Vec::new(),
                drive_id,
                permissions: Vec::new(),
            },
        );
        match bytes {
            Some(bytes) => self.add_revision(&id, bytes),
            None => self.touch(&id),
        }
        id
    }

    fn add_revision(&mut self, id: &str, bytes: Vec<u8>) {
        let rev_id = format!("0Brev{:010}", self.next());
        let version = self.bump();
        let Some(node) = self.files.get_mut(id) else {
            return;
        };
        node.revisions.push(Rev {
            id: rev_id,
            bytes,
            modified: SystemTime::now(),
            keep_forever: false,
        });
        node.version = version;
        // Drive prunes the oldest unpinned revisions, never the head.
        while node.revisions.iter().filter(|r| !r.keep_forever).count() > MAX_REVISIONS {
            let last = node.revisions.len() - 1;
            match node
                .revisions
                .iter()
                .position(|r| !r.keep_forever)
                .filter(|i| *i != last)
            {
                Some(oldest) => {
                    node.revisions.remove(oldest);
                }
                None => break,
            }
        }
        self.touch(id);
    }

    fn children(&self, id: &str) -> Vec<String> {
        self.files
            .values()
            .filter(|n| n.parents.iter().any(|p| p == id))
            .map(|n| n.id.clone())
            .collect()
    }

    fn descendants(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![id.to_owned()];
        while let Some(next) = pending.pop() {
            for child in self.children(&next) {
                if seen.insert(child.clone()) {
                    out.push(child.clone());
                    pending.push(child);
                }
            }
        }
        out
    }

    fn set_trashed(&mut self, id: &str, trashed: bool) {
        let mut affected = vec![id.to_owned()];
        affected.extend(self.descendants(id));
        for (index, target) in affected.iter().enumerate() {
            let version = self.bump();
            let Some(node) = self.files.get_mut(target) else {
                continue;
            };
            if index == 0 {
                node.trashed = trashed;
                node.explicitly_trashed = trashed;
            } else if trashed && !node.trashed {
                node.trashed = true;
            } else if !trashed && !node.explicitly_trashed {
                node.trashed = false;
            } else {
                continue;
            }
            node.version = version;
            self.touch(target);
        }
    }

    fn delete(&mut self, id: &str) {
        let mut gone = vec![id.to_owned()];
        gone.extend(self.descendants(id));
        for target in gone {
            self.files.remove(&target);
            self.touch(&target);
        }
    }

    fn file_json(node: &Node) -> Value {
        let mut map = Map::new();
        map.insert("kind".into(), json!("drive#file"));
        map.insert("id".into(), json!(node.id));
        map.insert("name".into(), json!(node.name));
        map.insert("mimeType".into(), json!(node.mime));
        if !node.parents.is_empty() {
            map.insert("parents".into(), json!(node.parents));
        }
        map.insert("trashed".into(), json!(node.trashed));
        map.insert("explicitlyTrashed".into(), json!(node.explicitly_trashed));
        map.insert("version".into(), json!(node.version.to_string()));
        map.insert("modifiedTime".into(), json!(format_rfc3339(node.modified)));
        let link = match node.mime.strip_prefix(GOOGLE_APPS) {
            Some("document") => format!(
                "https://docs.google.com/document/d/{}/edit?usp=drivesdk",
                node.id
            ),
            Some("spreadsheet") => format!(
                "https://docs.google.com/spreadsheets/d/{}/edit?usp=drivesdk",
                node.id
            ),
            Some("presentation") => format!(
                "https://docs.google.com/presentation/d/{}/edit?usp=drivesdk",
                node.id
            ),
            Some("folder") => format!("https://drive.google.com/drive/folders/{}", node.id),
            _ => format!(
                "https://drive.google.com/file/d/{}/view?usp=drivesdk",
                node.id
            ),
        };
        map.insert("webViewLink".into(), json!(link));
        if let Some(drive) = &node.drive_id {
            map.insert("driveId".into(), json!(drive));
        }
        if node.is_blob()
            && let Some(head) = node.head()
        {
            map.insert("size".into(), json!(head.bytes.len().to_string()));
            map.insert("headRevisionId".into(), json!(head.id));
            map.insert(
                "md5Checksum".into(),
                json!(hex(&md5::Md5::digest(&head.bytes))),
            );
            map.insert(
                "sha1Checksum".into(),
                json!(hex(&sha1::Sha1::digest(&head.bytes))),
            );
            map.insert(
                "sha256Checksum".into(),
                json!(hex(&sha2::Sha256::digest(&head.bytes))),
            );
        }
        Value::Object(map)
    }
}

// --- request handling ----------------------------------------------------

struct Call<'a> {
    method: &'a str,
    segments: Vec<String>,
    query: BTreeMap<String, String>,
    request: &'a Request,
    host: String,
}

impl Call<'_> {
    fn param(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(String::as_str)
    }

    fn flag(&self, name: &str) -> bool {
        self.param(name) == Some("true")
    }

    fn body_json(&self) -> Option<Value> {
        if self.request.body.is_empty() {
            return Some(Value::Object(Map::new()));
        }
        serde_json::from_slice(&self.request.body).ok()
    }
}

fn handle(state: &Mutex<State>, request: &Request) -> Option<Reply> {
    let url = Url::parse(&format!("http://fake{}", request.target)).ok()?;
    let segments: Vec<String> = url
        .path_segments()
        .map(|s| s.map(ToOwned::to_owned).collect())
        .unwrap_or_default();
    let query: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
    let call = Call {
        method: request.method.as_str(),
        segments,
        query,
        request,
        host: request.header("host").unwrap_or("127.0.0.1").to_owned(),
    };
    let token = request
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "));
    let mut state = state.lock().expect("fake drive state");
    let upload_session = call.param("upload_id").is_some();
    let source = token == Some(SOURCE_TOKEN);
    if source {
        if state.offline {
            return None;
        }
        if let Some(fault) = state.faults.pop_front() {
            return Some(fault_reply(fault));
        }
    }
    match token {
        Some(SOURCE_TOKEN | REMOTE_TOKEN) => {}
        // The session URI authorises its own chunks.
        None if upload_session => {}
        _ => {
            return Some(error(
                401,
                "authError",
                "Invalid Credentials",
                Some("Authorization"),
            ));
        }
    }
    Some(route(&mut state, &call, source))
}

fn fault_reply(fault: FakeFault) -> Reply {
    match fault {
        FakeFault::UserRateLimited => error(
            403,
            "userRateLimitExceeded",
            "User Rate Limit Exceeded",
            None,
        ),
        FakeFault::TooManyRequests => {
            error(429, "rateLimitExceeded", "Rate Limit Exceeded", None).header("Retry-After", "1")
        }
        FakeFault::ServerError => error(503, "backendError", "Backend Error", None),
        FakeFault::ExpiredCredential => error(
            401,
            "authError",
            "Invalid Credentials",
            Some("Authorization"),
        ),
        FakeFault::TruncatedBody => error(500, "internalError", "unused", None),
    }
}

fn route(state: &mut State, call: &Call<'_>, source: bool) -> Reply {
    let segments: Vec<&str> = call.segments.iter().map(String::as_str).collect();
    match (call.method, segments.as_slice()) {
        ("GET", ["drive", "v3", "about"]) => about(state, call),
        ("GET", ["drive", "v3", "changes", "startPageToken"]) => json_reply(
            200,
            &json!({"kind": "drive#startPageToken", "startPageToken": state.seq.to_string()}),
        ),
        ("GET", ["drive", "v3", "changes"]) => changes(state, call),
        ("GET", ["drive", "v3", "files"]) => list(state, call),
        ("POST", ["drive", "v3", "files"]) => create(state, call),
        ("GET", ["drive", "v3", "files", id]) => get(state, call, id, source),
        ("PATCH", ["drive", "v3", "files", id]) => update(state, call, id),
        ("DELETE", ["drive", "v3", "files", id]) => delete(state, call, id),
        ("GET", ["drive", "v3", "files", id, "export"]) => export(state, call, id),
        ("GET", ["drive", "v3", "files", id, "revisions"]) => revisions(state, call, id),
        ("GET", ["drive", "v3", "files", id, "revisions", rev]) => {
            revision(state, call, id, rev, source)
        }
        ("PATCH", ["drive", "v3", "files", id, "revisions", rev]) => {
            update_revision(state, call, id, rev)
        }
        ("POST", ["drive", "v3", "files", id, "permissions"]) => permission(state, call, id),
        ("POST", ["upload", "drive", "v3", "files"]) if call.param("upload_id").is_none() => {
            open_session(state, call, None)
        }
        ("PATCH", ["upload", "drive", "v3", "files", id]) if call.param("upload_id").is_none() => {
            open_session(state, call, Some(id))
        }
        ("PUT", ["upload", "drive", "v3", "files", ..]) => upload_chunk(state, call),
        ("DELETE", ["upload", "drive", "v3", "files", ..]) => {
            let id = call.param("upload_id").unwrap_or_default();
            if state.sessions.remove(id).is_some() {
                Reply::status(499)
            } else {
                not_found("upload session")
            }
        }
        _ => error(404, "notFound", "Not Found", None),
    }
}

fn error(status: u16, reason: &str, message: &str, location: Option<&str>) -> Reply {
    let mut item = json!({"domain": "global", "reason": reason, "message": message});
    if let Some(location) = location {
        item["location"] = json!(location);
        item["locationType"] = json!(if location == "Authorization" {
            "header"
        } else {
            "parameter"
        });
    }
    json_reply(
        status,
        &json!({"error": {"errors": [item], "code": status, "message": message}}),
    )
}

fn not_found(id: &str) -> Reply {
    error(
        404,
        "notFound",
        &format!("File not found: {id}."),
        Some("fileId"),
    )
}

fn json_reply(status: u16, value: &Value) -> Reply {
    Reply::status(status)
        .header("Content-Type", "application/json; charset=UTF-8")
        .body(serde_json::to_vec(value).expect("json"))
}

fn about(_state: &State, call: &Call<'_>) -> Reply {
    let Some(fields) = call.param("fields") else {
        return error(
            400,
            "required",
            "The 'fields' parameter is required for this operation.",
            Some("fields"),
        );
    };
    let value = json!({
        "kind": "drive#about",
        "user": {"kind": "drive#user", "displayName": "Ada Lovelace", "emailAddress": "ada@example.com"},
        "storageQuota": {
            "limit": "16106127360",
            "usage": "5368709120",
            "usageInDrive": "4294967296",
            "usageInDriveTrash": "1048576"
        }
    });
    select_reply(&value, Some(fields), "")
}

/// Look up a live item (or a trashed one: Drive answers `files.get` for
/// items in the trash).
fn find<'a>(state: &'a State, id: &str, call: &Call<'_>) -> Result<&'a Node, Reply> {
    find_in(state, id, Some(call))
}

/// [`find`] for methods without `supportsAllDrives` (revisions, export).
fn find_any<'a>(state: &'a State, id: &str) -> Result<&'a Node, Reply> {
    find_in(state, id, None)
}

fn find_in<'a>(state: &'a State, id: &str, call: Option<&Call<'_>>) -> Result<&'a Node, Reply> {
    let id = state.resolve(id);
    let node = state.files.get(&id).ok_or_else(|| not_found(&id))?;
    if node.drive_id.is_some() && call.is_some_and(|call| !call.flag("supportsAllDrives")) {
        return Err(not_found(&id));
    }
    Ok(node)
}

fn get(state: &mut State, call: &Call<'_>, id: &str, source: bool) -> Reply {
    let node = match find(state, id, call) {
        Ok(node) => node.clone(),
        Err(reply) => return reply,
    };
    if call.param("alt") == Some("media") {
        state.stats.media_gets += 1;
        if !node.is_blob() {
            return error(
                403,
                "fileNotDownloadable",
                "Only files with binary content can be downloaded. Use Export with Docs Editors files.",
                Some("alt"),
            );
        }
        let bytes = node.head().map(|r| r.bytes.clone()).unwrap_or_default();
        let truncate = source && std::mem::take(&mut state.truncate);
        return media(call, &bytes, &node.mime, truncate);
    }
    select_reply(&State::file_json(&node), call.param("fields"), FILE_DEFAULT)
}

const FILE_DEFAULT: &str = "kind,id,name,mimeType";

fn media(call: &Call<'_>, bytes: &[u8], mime: &str, truncate: bool) -> Reply {
    let len = bytes.len();
    let (status, body, range_header) = match call.request.header("range").and_then(parse_range) {
        Some((start, end)) => {
            if start >= len {
                return error(
                    416,
                    "requestedRangeNotSatisfiable",
                    "Request range not satisfiable",
                    None,
                );
            }
            let end = end.map_or(len, |e| (e + 1).min(len));
            (
                206,
                bytes[start..end].to_vec(),
                Some(format!("bytes {start}-{}/{len}", end - 1)),
            )
        }
        None => (200, bytes.to_vec(), None),
    };
    let mut reply = Reply::status(status).header("Content-Type", mime);
    if let Some(range) = range_header {
        reply = reply.header("Content-Range", &range);
    }
    if truncate {
        let half = body.len() / 2;
        return reply
            .header("Content-Length", &body.len().to_string())
            .body(body[..half].to_vec())
            .without_content_length();
    }
    reply.body(body)
}

fn parse_range(value: &str) -> Option<(usize, Option<usize>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start = start.parse().ok()?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

fn create(state: &mut State, call: &Call<'_>) -> Reply {
    let Some(body) = call.body_json() else {
        return error(400, "parseError", "Parse Error", None);
    };
    let name = body["name"].as_str().unwrap_or("Untitled").to_owned();
    let mime = body["mimeType"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let parents = match parents_of(state, call, &body) {
        Ok(parents) => parents,
        Err(reply) => return reply,
    };
    let bytes = (!mime.starts_with(GOOGLE_APPS)).then(Vec::new);
    let id = state.create_node(&name, &mime, parents, bytes);
    let node = state.files[&id].clone();
    select_reply(&State::file_json(&node), call.param("fields"), FILE_DEFAULT)
}

/// The `parents` of a create request, defaulting to My Drive's root.
fn parents_of(state: &State, call: &Call<'_>, body: &Value) -> Result<Vec<String>, Reply> {
    let parents: Vec<String> = match body["parents"].as_array() {
        Some(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(|p| state.resolve(p))
            .collect(),
        None => vec![state.root.clone()],
    };
    if parents.len() > 1 {
        return Err(error(
            403,
            "cannotAddParent",
            "Increasing the number of parents is not allowed.",
            None,
        ));
    }
    for parent in &parents {
        match state.files.get(parent) {
            Some(node) if node.is_folder() && !node.trashed => {
                if node.drive_id.is_some() && !call.flag("supportsAllDrives") {
                    return Err(not_found(parent));
                }
            }
            _ => return Err(not_found(parent)),
        }
    }
    Ok(parents)
}

fn update(state: &mut State, call: &Call<'_>, id: &str) -> Reply {
    let node = match find(state, id, call) {
        Ok(node) => node.clone(),
        Err(reply) => return reply,
    };
    let Some(body) = call.body_json() else {
        return error(400, "parseError", "Parse Error", None);
    };
    let mut parents = node.parents.clone();
    if let Some(remove) = call.param("removeParents") {
        let remove: Vec<String> = remove.split(',').map(|p| state.resolve(p)).collect();
        parents.retain(|p| !remove.contains(p));
    }
    if let Some(add) = call.param("addParents") {
        for parent in add.split(',').map(|p| state.resolve(p)) {
            match state.files.get(&parent) {
                Some(folder) if folder.is_folder() => {}
                _ => return not_found(&parent),
            }
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
    }
    if parents.len() > node.parents.len() && parents.len() > 1 {
        return error(
            403,
            "cannotAddParent",
            "Increasing the number of parents is not allowed.",
            None,
        );
    }
    let trashed = body["trashed"].as_bool();
    let version = state.bump();
    {
        let target = state.files.get_mut(&node.id).expect("found above");
        if let Some(name) = body["name"].as_str() {
            target.name = name.to_owned();
        }
        target.parents = parents;
        target.version = version;
    }
    state.touch(&node.id);
    if let Some(trashed) = trashed
        && trashed != node.trashed
    {
        state.set_trashed(&node.id, trashed);
    }
    let node = state.files[&node.id].clone();
    select_reply(&State::file_json(&node), call.param("fields"), FILE_DEFAULT)
}

fn delete(state: &mut State, call: &Call<'_>, id: &str) -> Reply {
    let node = match find(state, id, call) {
        Ok(node) => node.id.clone(),
        Err(reply) => return reply,
    };
    state.delete(&node);
    Reply::status(204)
}

fn export(state: &State, call: &Call<'_>, id: &str) -> Reply {
    let node = match find_any(state, id) {
        Ok(node) => node,
        Err(reply) => return reply,
    };
    if !node.is_native() {
        return error(
            403,
            "fileNotExportable",
            "Export only supports Docs Editors files.",
            None,
        );
    }
    let Some(mime) = call.param("mimeType") else {
        return error(
            400,
            "required",
            "Required parameter: mimeType",
            Some("mimeType"),
        );
    };
    let formats = api::native_document(&node.mime, None).exports;
    if !formats.iter().any(|f| f.mime == mime) {
        return error(
            400,
            "badRequest",
            "The requested conversion is not supported.",
            Some("mimeType"),
        );
    }
    Reply::status(200)
        .header("Content-Type", mime)
        .body(format!("{} exported as {mime}", node.name))
}

fn revision_json(rev: &Rev, mime: &str) -> Value {
    json!({
        "kind": "drive#revision",
        "id": rev.id,
        "mimeType": mime,
        "modifiedTime": format_rfc3339(rev.modified),
        "keepForever": rev.keep_forever,
        "size": rev.bytes.len().to_string(),
        "lastModifyingUser": {"kind": "drive#user", "displayName": "Ada Lovelace", "emailAddress": "ada@example.com"},
    })
}

fn revisions(state: &State, call: &Call<'_>, id: &str) -> Reply {
    let node = match find_any(state, id) {
        Ok(node) => node,
        Err(reply) => return reply,
    };
    let size = page_size(call, 200, 1000);
    let start: usize = match call.param("pageToken").map(str::parse) {
        None => 0,
        Some(Ok(start)) => start,
        Some(Err(_)) => return error(400, "invalid", "Invalid Value", Some("pageToken")),
    };
    let all: Vec<Value> = node
        .revisions
        .iter()
        .map(|r| revision_json(r, &node.mime))
        .collect();
    let page: Vec<Value> = all.iter().skip(start).take(size).cloned().collect();
    let mut value = json!({"kind": "drive#revisionList", "revisions": page});
    if start + size < all.len() {
        value["nextPageToken"] = json!((start + size).to_string());
    }
    select_reply(
        &value,
        call.param("fields"),
        "kind,nextPageToken,revisions(kind,id,mimeType,modifiedTime)",
    )
}

fn revision(state: &mut State, call: &Call<'_>, id: &str, rev: &str, source: bool) -> Reply {
    let node = match find_any(state, id) {
        Ok(node) => node.clone(),
        Err(reply) => return reply,
    };
    let Some(found) = node.revisions.iter().find(|r| r.id == rev) else {
        return error(
            404,
            "notFound",
            &format!("Revision not found: {rev}."),
            Some("revisionId"),
        );
    };
    if call.param("alt") == Some("media") {
        state.stats.media_gets += 1;
        let truncate = source && std::mem::take(&mut state.truncate);
        return media(call, &found.bytes, &node.mime, truncate);
    }
    select_reply(
        &revision_json(found, &node.mime),
        call.param("fields"),
        "kind,id,mimeType,modifiedTime",
    )
}

fn update_revision(state: &mut State, call: &Call<'_>, id: &str, rev: &str) -> Reply {
    let id = match find_any(state, id) {
        Ok(node) => node.id.clone(),
        Err(reply) => return reply,
    };
    let keep = call.body_json().and_then(|b| b["keepForever"].as_bool());
    let node = state.files.get_mut(&id).expect("found above");
    let Some(found) = node.revisions.iter_mut().find(|r| r.id == rev) else {
        return error(
            404,
            "notFound",
            &format!("Revision not found: {rev}."),
            Some("revisionId"),
        );
    };
    if let Some(keep) = keep {
        found.keep_forever = keep;
    }
    let value = revision_json(found, &node.mime);
    select_reply(
        &value,
        call.param("fields"),
        "kind,id,mimeType,modifiedTime",
    )
}

fn permission(state: &mut State, call: &Call<'_>, id: &str) -> Reply {
    let id = match find(state, id, call) {
        Ok(node) => node.id.clone(),
        Err(reply) => return reply,
    };
    let Some(body) = call.body_json() else {
        return error(400, "parseError", "Parse Error", None);
    };
    let (Some(kind), Some(role)) = (body["type"].as_str(), body["role"].as_str()) else {
        return error(400, "required", "Required: type and role", None);
    };
    let permission_id = if kind == "anyone" {
        "anyoneWithLink".to_owned()
    } else {
        format!("perm{}", state.next())
    };
    let value =
        json!({"kind": "drive#permission", "id": permission_id, "type": kind, "role": role});
    let node = state.files.get_mut(&id).expect("found above");
    node.permissions.retain(|p| p["id"] != value["id"]);
    node.permissions.push(value.clone());
    select_reply(&value, call.param("fields"), "kind,id,type,role")
}

fn page_size(call: &Call<'_>, default: usize, max: usize) -> usize {
    call.param("pageSize")
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

fn list(state: &mut State, call: &Call<'_>) -> Reply {
    state.stats.file_lists += 1;
    let query = match call.param("q").map(parse_query).transpose() {
        Ok(query) => query,
        Err(()) => return error(400, "invalid", "Invalid Value", Some("q")),
    };
    let drive = match call.param("corpora") {
        Some("drive") => match call.param("driveId") {
            Some(drive)
                if call.flag("includeItemsFromAllDrives") && call.flag("supportsAllDrives") =>
            {
                Some(drive.to_owned())
            }
            _ => {
                return error(
                    403,
                    "teamDrivesParameterRequired",
                    "The driveId, includeItemsFromAllDrives and supportsAllDrives parameters are required.",
                    None,
                );
            }
        },
        _ => None,
    };
    let all_drives = call.flag("includeItemsFromAllDrives");
    let matching: Vec<&Node> = state
        .files
        .values()
        .filter(|n| n.id != state.root)
        .filter(|n| match &drive {
            Some(drive) => n.drive_id.as_deref() == Some(drive.as_str()) && n.id != *drive,
            None => n.drive_id.is_none() || all_drives,
        })
        .filter(|n| query.as_ref().is_none_or(|q| q.eval(n, &state.root)))
        .collect();
    let size = page_size(call, 100, 1000);
    let start: usize = match call.param("pageToken") {
        None => 0,
        Some(token) => match token.strip_prefix("~!!~").and_then(|t| t.parse().ok()) {
            Some(start) => start,
            None => return error(400, "invalid", "Invalid Value", Some("pageToken")),
        },
    };
    let files: Vec<Value> = matching
        .iter()
        .skip(start)
        .take(size)
        .map(|n| State::file_json(n))
        .collect();
    let mut value = json!({"kind": "drive#fileList", "incompleteSearch": false, "files": files});
    if start + size < matching.len() {
        value["nextPageToken"] = json!(format!("~!!~{}", start + size));
    }
    select_reply(
        &value,
        call.param("fields"),
        "kind,nextPageToken,incompleteSearch,files(kind,id,name,mimeType)",
    )
}

fn changes(state: &mut State, call: &Call<'_>) -> Reply {
    state.stats.change_lists += 1;
    let Some(token) = call.param("pageToken") else {
        return error(
            400,
            "required",
            "Required parameter: pageToken",
            Some("pageToken"),
        );
    };
    let Some(token) = token
        .parse::<u64>()
        .ok()
        .filter(|t| *t >= state.floor && *t <= state.seq)
    else {
        return error(400, "invalid", "Invalid Value", Some("pageToken"));
    };
    let drive = call.param("driveId").map(ToOwned::to_owned);
    if drive.is_some() && !call.flag("supportsAllDrives") {
        return error(
            403,
            "teamDrivesParameterRequired",
            "supportsAllDrives is required.",
            None,
        );
    }
    let all_drives = call.flag("includeItemsFromAllDrives");
    let include_removed = call.param("includeRemoved") != Some("false");
    // Each file once, at its latest change.
    let mut latest: BTreeMap<&str, u64> = BTreeMap::new();
    for (seq, id) in &state.log {
        if *seq >= token {
            latest.insert(id.as_str(), *seq);
        }
    }
    let mut entries: Vec<(u64, &str)> = latest.into_iter().map(|(id, seq)| (seq, id)).collect();
    entries.sort_unstable();
    let entries: Vec<(u64, &str)> = entries
        .into_iter()
        .filter(|(_, id)| match state.files.get(*id) {
            None => include_removed,
            Some(node) => match (&drive, &node.drive_id) {
                (Some(drive), Some(of)) => drive == of,
                (Some(_), None) => false,
                (None, Some(_)) => all_drives,
                (None, None) => true,
            },
        })
        .collect();
    let size = page_size(call, 100, 1000);
    let page: Vec<Value> = entries
        .iter()
        .take(size)
        .map(|(_, id)| match state.files.get(*id) {
            Some(node) => json!({
                "kind": "drive#change",
                "changeType": "file",
                "time": format_rfc3339(node.modified),
                "removed": false,
                "fileId": id,
                "file": State::file_json(node),
            }),
            None => json!({
                "kind": "drive#change",
                "changeType": "file",
                "time": format_rfc3339(SystemTime::now()),
                "removed": true,
                "fileId": id,
            }),
        })
        .collect();
    let mut value = json!({"kind": "drive#changeList", "changes": page});
    match entries.get(size) {
        Some((seq, _)) => value["nextPageToken"] = json!(seq.to_string()),
        None => value["newStartPageToken"] = json!(state.seq.to_string()),
    }
    select_reply(
        &value,
        call.param("fields"),
        "kind,nextPageToken,newStartPageToken,changes(kind,changeType,time,removed,fileId,file(kind,id,name,mimeType))",
    )
}

// --- resumable uploads ---------------------------------------------------

fn open_session(state: &mut State, call: &Call<'_>, target: Option<&str>) -> Reply {
    if call.param("uploadType") != Some("resumable") {
        return error(
            400,
            "badRequest",
            "Unsupported upload type",
            Some("uploadType"),
        );
    }
    let Some(metadata) = call.body_json() else {
        return error(400, "parseError", "Parse Error", None);
    };
    let target = match target {
        Some(id) => match find(state, id, call) {
            Ok(node) if node.is_blob() && !node.is_folder() => Some(node.id.clone()),
            Ok(_) => {
                return error(
                    403,
                    "fileNotUploadable",
                    "This file cannot be updated with content.",
                    None,
                );
            }
            Err(reply) => return reply,
        },
        None => {
            if let Err(reply) = parents_of(state, call, &metadata) {
                return reply;
            }
            None
        }
    };
    let total = call
        .request
        .header("x-upload-content-length")
        .and_then(|v| v.parse().ok());
    let session_id = format!("ADPycdt{:010}", state.next());
    state.sessions.insert(
        session_id.clone(),
        Session {
            target,
            metadata,
            total,
            data: Vec::new(),
            fields: call.param("fields").map(ToOwned::to_owned),
            completed: None,
        },
    );
    let path = call.segments.join("/");
    let mut location = format!(
        "http://{}/{path}?uploadType=resumable&upload_id={session_id}",
        call.host
    );
    if call.flag("supportsAllDrives") {
        location.push_str("&supportsAllDrives=true");
    }
    Reply::status(200).header("Location", &location)
}

fn upload_chunk(state: &mut State, call: &Call<'_>) -> Reply {
    state.stats.upload_puts += 1;
    let Some(session_id) = call.param("upload_id").map(ToOwned::to_owned) else {
        return error(400, "badRequest", "Missing upload_id", None);
    };
    let Some(session) = state.sessions.get(&session_id) else {
        return not_found("upload session");
    };
    if let Some(done) = session.completed.clone() {
        let fields = session.fields.clone();
        return committed_reply(state, &done, fields.as_deref());
    }
    let Some(range) = call.request.header("content-range") else {
        return error(400, "badRequest", "Missing Content-Range", None);
    };
    let Some((span, total)) = range.strip_prefix("bytes ").and_then(|r| r.split_once('/')) else {
        return error(400, "badRequest", "Invalid Content-Range", None);
    };
    let total: Option<u64> = if total == "*" {
        None
    } else {
        total.parse().ok()
    };
    if let (Some(total), Some(declared)) = (total, session.total)
        && total != declared
    {
        return error(
            400,
            "badRequest",
            "Content-Range total differs from X-Upload-Content-Length",
            None,
        );
    }
    let committed = session.data.len();
    if span == "*" {
        // A status query.
        if total == Some(0) {
            return commit(state, &session_id);
        }
        return incomplete(committed);
    }
    let Some((first, last)) = span
        .split_once('-')
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
    else {
        return error(400, "badRequest", "Invalid Content-Range", None);
    };
    let body = &call.request.body;
    if last < first || last - first + 1 != body.len() {
        return error(
            400,
            "badRequest",
            "Content-Range does not match the body",
            None,
        );
    }
    if first > committed {
        return error(
            400,
            "badRequest",
            "The upload offset is past the committed bytes",
            None,
        );
    }
    let fresh = &body[committed - first..];
    let is_final = total.is_some_and(|t| (last + 1) as u64 == t);
    let keep = if is_final {
        fresh.len()
    } else {
        // Non-final chunks are committed in whole quanta only.
        let quantum = UPLOAD_QUANTUM as usize;
        ((committed + fresh.len()) / quantum * quantum).saturating_sub(committed)
    };
    let session = state
        .sessions
        .get_mut(&session_id)
        .expect("looked up above");
    session.data.extend_from_slice(&fresh[..keep]);
    let committed = session.data.len();
    if total.is_some_and(|t| committed as u64 == t) {
        return commit(state, &session_id);
    }
    incomplete(committed)
}

fn incomplete(committed: usize) -> Reply {
    let reply = Reply::status(308);
    if committed == 0 {
        reply
    } else {
        reply.header("Range", &format!("bytes=0-{}", committed - 1))
    }
}

fn commit(state: &mut State, session_id: &str) -> Reply {
    let session = state.sessions.get(session_id).expect("known session");
    let data = session.data.clone();
    let metadata = session.metadata.clone();
    let fields = session.fields.clone();
    let target = session.target.clone();
    let id = match target {
        Some(id) => {
            if !state.files.contains_key(&id) {
                return not_found(&id);
            }
            if let Some((raced, bytes)) = state.race.take() {
                if raced == id {
                    state.add_revision(&id, bytes);
                } else {
                    state.race = Some((raced, bytes));
                }
            }
            if let Some(name) = metadata["name"].as_str() {
                let node = state.files.get_mut(&id).expect("checked above");
                node.name = name.to_owned();
            }
            state.add_revision(&id, data);
            id
        }
        None => {
            let name = metadata["name"].as_str().unwrap_or("Untitled").to_owned();
            let mime = metadata["mimeType"]
                .as_str()
                .unwrap_or("application/octet-stream")
                .to_owned();
            let parents: Vec<String> = match metadata["parents"].as_array() {
                Some(list) => list
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|p| state.resolve(p))
                    .collect(),
                None => vec![state.root.clone()],
            };
            state.create_node(&name, &mime, parents, Some(data))
        }
    };
    if let Some(session) = state.sessions.get_mut(session_id) {
        session.completed = Some(id.clone());
        session.data = Vec::new();
    }
    committed_reply(state, &id, fields.as_deref())
}

fn committed_reply(state: &State, id: &str, fields: Option<&str>) -> Reply {
    match state.files.get(id) {
        Some(node) => select_reply(&State::file_json(node), fields, FILE_DEFAULT),
        None => not_found(id),
    }
}

// --- fields selection ----------------------------------------------------

#[derive(Debug)]
struct Field {
    name: String,
    sub: Option<Vec<Field>>,
}

fn select_reply(value: &Value, fields: Option<&str>, default: &str) -> Reply {
    let spec = fields.unwrap_or(default);
    match parse_fields(spec) {
        Some(selection) => json_reply(200, &select(value, &selection)),
        None => error(
            400,
            "invalidParameter",
            "Invalid field selection",
            Some("fields"),
        ),
    }
}

fn parse_fields(spec: &str) -> Option<Vec<Field>> {
    let chars: Vec<char> = spec.chars().filter(|c| !c.is_whitespace()).collect();
    let mut at = 0;
    let fields = parse_field_list(&chars, &mut at)?;
    (at == chars.len()).then_some(fields)
}

fn parse_field_list(chars: &[char], at: &mut usize) -> Option<Vec<Field>> {
    let mut out = Vec::new();
    loop {
        let start = *at;
        while chars
            .get(*at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '*')
        {
            *at += 1;
        }
        let name: String = chars.get(start..*at)?.iter().collect();
        if name.is_empty() {
            return None;
        }
        let sub = match chars.get(*at) {
            Some('(') => {
                *at += 1;
                let sub = parse_field_list(chars, at)?;
                if chars.get(*at) != Some(&')') {
                    return None;
                }
                *at += 1;
                Some(sub)
            }
            Some('/') => {
                *at += 1;
                let nested = parse_field_list(chars, at)?;
                Some(nested)
            }
            _ => None,
        };
        out.push(Field { name, sub });
        match chars.get(*at) {
            Some(',') => *at += 1,
            _ => return Some(out),
        }
    }
}

fn select(value: &Value, fields: &[Field]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| select(v, fields)).collect()),
        Value::Object(map) => {
            if fields.iter().any(|f| f.name == "*" && f.sub.is_none()) {
                return value.clone();
            }
            let mut out = Map::new();
            for field in fields {
                if let Some(inner) = map.get(&field.name) {
                    let picked = match &field.sub {
                        Some(sub) => select(inner, sub),
                        None => inner.clone(),
                    };
                    out.insert(field.name.clone(), picked);
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// --- search queries ------------------------------------------------------

#[derive(Debug)]
enum Query {
    InParents(String),
    Field {
        name: String,
        negate: bool,
        value: String,
    },
    Not(Box<Query>),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
}

impl Query {
    fn eval(&self, node: &Node, root: &str) -> bool {
        match self {
            Self::InParents(id) => {
                let id = if id == "root" { root } else { id.as_str() };
                node.parents.iter().any(|p| p == id)
            }
            Self::Field {
                name,
                negate,
                value,
            } => {
                let actual = match name.as_str() {
                    "trashed" => node.trashed.to_string(),
                    "name" => node.name.clone(),
                    "mimeType" => node.mime.clone(),
                    _ => return false,
                };
                (actual == *value) != *negate
            }
            Self::Not(inner) => !inner.eval(node, root),
            Self::And(a, b) => a.eval(node, root) && b.eval(node, root),
            Self::Or(a, b) => a.eval(node, root) || b.eval(node, root),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Open,
    Close,
    Text(String),
    Word(String),
    Eq,
    Ne,
}

fn tokenize(q: &str) -> Result<Vec<Token>, ()> {
    let mut tokens = Vec::new();
    let mut chars = q.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {}
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            '=' => tokens.push(Token::Eq),
            '!' if chars.peek() == Some(&'=') => {
                chars.next();
                tokens.push(Token::Ne);
            }
            '\'' => {
                let mut text = String::new();
                loop {
                    match chars.next().ok_or(())? {
                        '\\' => text.push(chars.next().ok_or(())?),
                        '\'' => break,
                        other => text.push(other),
                    }
                }
                tokens.push(Token::Text(text));
            }
            c if c.is_ascii_alphanumeric() => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphanumeric() {
                        word.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Word(word));
            }
            _ => return Err(()),
        }
    }
    Ok(tokens)
}

fn parse_query(q: &str) -> Result<Query, ()> {
    let tokens = tokenize(q)?;
    let mut at = 0;
    let query = parse_or(&tokens, &mut at)?;
    if at == tokens.len() {
        Ok(query)
    } else {
        Err(())
    }
}

fn word(tokens: &[Token], at: usize, expected: &str) -> bool {
    matches!(tokens.get(at), Some(Token::Word(w)) if w == expected)
}

fn parse_or(tokens: &[Token], at: &mut usize) -> Result<Query, ()> {
    let mut left = parse_and(tokens, at)?;
    while word(tokens, *at, "or") {
        *at += 1;
        let right = parse_and(tokens, at)?;
        left = Query::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(tokens: &[Token], at: &mut usize) -> Result<Query, ()> {
    let mut left = parse_unary(tokens, at)?;
    while word(tokens, *at, "and") {
        *at += 1;
        let right = parse_unary(tokens, at)?;
        left = Query::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_unary(tokens: &[Token], at: &mut usize) -> Result<Query, ()> {
    match tokens.get(*at) {
        Some(Token::Word(w)) if w == "not" => {
            *at += 1;
            Ok(Query::Not(Box::new(parse_unary(tokens, at)?)))
        }
        Some(Token::Open) => {
            *at += 1;
            let inner = parse_or(tokens, at)?;
            if tokens.get(*at) != Some(&Token::Close) {
                return Err(());
            }
            *at += 1;
            Ok(inner)
        }
        Some(Token::Text(id)) => {
            if word(tokens, *at + 1, "in") && word(tokens, *at + 2, "parents") {
                *at += 3;
                Ok(Query::InParents(id.clone()))
            } else {
                Err(())
            }
        }
        Some(Token::Word(name)) if ["trashed", "name", "mimeType"].contains(&name.as_str()) => {
            let negate = match tokens.get(*at + 1) {
                Some(Token::Eq) => false,
                Some(Token::Ne) => true,
                _ => return Err(()),
            };
            let value = match tokens.get(*at + 2) {
                Some(Token::Text(text)) if name != "trashed" => text.clone(),
                Some(Token::Word(w)) if name == "trashed" && (w == "true" || w == "false") => {
                    w.clone()
                }
                _ => return Err(()),
            };
            *at += 3;
            Ok(Query::Field {
                name: name.clone(),
                negate,
                value,
            })
        }
        _ => Err(()),
    }
}

// --- helpers -------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// `2024-05-01T12:30:45.123Z`.
fn format_rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_the_client_parser() {
        let now = SystemTime::now();
        let text = format_rfc3339(now);
        let parsed = api::parse_rfc3339(&text).unwrap();
        let diff = now.duration_since(parsed).unwrap();
        assert!(diff.as_millis() < 1, "{text}");
    }

    #[test]
    fn field_selections_nest() {
        let value = json!({"kind": "x", "files": [{"id": "1", "name": "a", "size": "3"}], "nextPageToken": "t"});
        let picked = select(
            &value,
            &parse_fields("nextPageToken,files(id,size)").unwrap(),
        );
        assert_eq!(
            picked,
            json!({"files": [{"id": "1", "size": "3"}], "nextPageToken": "t"})
        );
        assert!(parse_fields("files(id").is_none());
    }

    #[test]
    fn queries_parse_the_client_subset() {
        let q = parse_query(
            r"('a' in parents or 'b' in parents) and trashed = false and name != 'it\'s'",
        )
        .unwrap();
        let node = Node {
            id: "x".into(),
            name: "plan".into(),
            mime: "text/plain".into(),
            parents: vec!["b".into()],
            trashed: false,
            explicitly_trashed: false,
            version: 1,
            modified: SystemTime::now(),
            revisions: Vec::new(),
            drive_id: None,
            permissions: Vec::new(),
        };
        assert!(q.eval(&node, "r"));
        assert!(parse_query("fullText contains 'x'").is_err());
    }
}
