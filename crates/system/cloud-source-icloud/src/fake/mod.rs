//! Apple's iCloud endpoints in memory, on the shared test responder.
//!
//! One loopback server answers all of them under path prefixes:
//!
//! - `/appleauth/…`: idmsa's SRP sign-in (a real SRP-6a verifier, so a
//!   wrong password fails the proof), the trusted-device and SMS code steps
//!   and `/2sv/trust`;
//! - `/setup/ws/1/…`: `accountLogin`, `validate`, `requestWebAccessState`;
//! - `/drivews/…`, `/docws/…`: listings, folder creation, rename, move,
//!   trash, download tokens, upload slots and document commits;
//! - `/content/…`: the content server (ranged downloads, multipart uploads).
//!
//! The shapes follow what rclone and pyicloud read from Apple. Faults can be
//! injected into the next request, sessions expired, Advanced Data
//! Protection switched on, and the server taken offline (connections are
//! dropped unanswered).
//!
//! Test support only (feature `fake-server`).

#![allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "a test fixture whose lock is poisoned has already failed the test"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "indexing a serde_json::Value by key yields Null, it never panics"
)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use test_http::{Reply, Request, Responder};
use tilo_crypto::srp_apple::{ELEMENT_LEN, server::ServerSession};
use url::Url;
use zeroize::Zeroizing;

use crate::ICloudConfig;
use crate::auth::{AccountInfo, Endpoints, StoredSession};
use crate::http::Cookies;

mod auth;
mod drive;

/// The Apple ID the fake knows.
pub const APPLE_ID: &str = "ada@example.com";
pub const PASSWORD: &str = "correct horse battery staple";
/// The code "pushed" to trusted devices.
pub const DEVICE_CODE: &str = "123456";
/// The code "texted" to the trusted phone.
pub const SMS_CODE: &str = "654321";
/// The trusted phone's id.
pub const PHONE_ID: u64 = 1;

/// A fault for the next matching request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFault {
    /// The next setup/drivews/docws request is answered 429 with
    /// `Retry-After: 1`.
    RateLimited,
    /// The next setup/drivews/docws request is answered 503.
    ServerError,
    /// The next content download declares its full length but sends half.
    TruncatedBody,
    /// The next content download sends half its body, declares no length
    /// and closes the connection.
    ClosedEarly,
    /// The next setup/drivews/docws request is answered 421, as if the web
    /// session had lapsed; it works again afterwards.
    ExpiredSession,
}

/// How the fake account behaves.
#[allow(clippy::struct_excessive_bools, reason = "independent switches")]
#[derive(Clone, Debug)]
pub struct Options {
    /// The account has trusted Apple devices (codes are pushed); without,
    /// codes go by SMS.
    pub trusted_devices: bool,
    /// Advanced Data Protection is on.
    pub advanced_data_protection: bool,
    /// Accepted codes are answered 409 with a session token, as idmsa has
    /// done since mid-2026.
    pub accept_code_with_conflict: bool,
    /// `update/documents` replaces an existing document in place. When
    /// false it refuses, as a server that only adds documents would.
    pub replace_in_place: bool,
    /// The password protocol offered: `s2k` or `s2k_fo`.
    pub protocol: &'static str,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            trusted_devices: true,
            advanced_data_protection: false,
            accept_code_with_conflict: false,
            replace_in_place: true,
            protocol: "s2k",
        }
    }
}

/// Request counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub validates: usize,
    pub listings: usize,
    pub full_downloads: usize,
    pub ranged_downloads: usize,
    pub uploads: usize,
    /// Codes pushed to trusted devices.
    pub pushes: usize,
    /// Codes sent by SMS.
    pub texts: usize,
}

#[derive(Clone, Debug)]
struct Node {
    folder: bool,
    zone: String,
    /// The full name (`name.extension`).
    name: String,
    /// The parent's docwsid; empty for the root.
    parent: String,
    etag: String,
    bytes: Vec<u8>,
    created: SystemTime,
    modified: SystemTime,
    trashed: bool,
}

struct SignInState {
    /// The server half of this sign-in's SRP exchange.
    srp: ServerSession,
    /// The client's `A`, as sent to `/signin/init`.
    a_pub: Option<Vec<u8>>,
    /// The idmsa session token issued at `/signin/complete`.
    session_token: Option<String>,
    code_verified: bool,
    texted: Option<u64>,
}

#[allow(clippy::struct_excessive_bools, reason = "independent server switches")]
struct State {
    options: Options,
    base: String,
    offline: bool,
    faults: VecDeque<FakeFault>,
    seq: u64,
    stats: Stats,
    // Sign-in.
    salt: Vec<u8>,
    iterations: u32,
    verifier: [u8; ELEMENT_LEN],
    sign_ins: HashMap<String, SignInState>,
    /// idmsa session tokens → trusted.
    session_tokens: HashMap<String, bool>,
    trust_tokens: HashSet<String>,
    web_tokens: HashSet<String>,
    // Drive.
    nodes: BTreeMap<String, Node>,
    /// Reserved upload slots: document id → zone.
    slots: HashMap<String, String>,
    /// Uploaded content by file checksum.
    uploads: HashMap<String, Vec<u8>>,
    /// Download tokens → document id.
    downloads: HashMap<String, String>,
}

impl State {
    fn next(&mut self, prefix: &str) -> String {
        self.seq = self.seq.saturating_add(1);
        format!("{prefix}{}", self.seq)
    }
}

/// A running fake. Dropping it stops the server.
pub struct FakeICloud {
    responder: Responder,
    state: Arc<Mutex<State>>,
}

impl FakeICloud {
    pub fn start() -> Self {
        Self::with(Options::default())
    }

    pub fn with(options: Options) -> Self {
        let salt = b"fake-icloud-salt".to_vec();
        let iterations = 1000;
        let verifier = auth::verifier(PASSWORD, &salt, iterations, options.protocol);
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "root".to_owned(),
            Node {
                folder: true,
                zone: crate::api::DEFAULT_ZONE.to_owned(),
                name: String::new(),
                parent: String::new(),
                etag: "1".to_owned(),
                bytes: Vec::new(),
                created: SystemTime::now(),
                modified: SystemTime::now(),
                trashed: false,
            },
        );
        let state = Arc::new(Mutex::new(State {
            options,
            base: String::new(),
            offline: false,
            faults: VecDeque::new(),
            seq: 1,
            stats: Stats::default(),
            salt,
            iterations,
            verifier,
            sign_ins: HashMap::new(),
            session_tokens: HashMap::new(),
            trust_tokens: HashSet::new(),
            web_tokens: HashSet::new(),
            nodes,
            slots: HashMap::new(),
            uploads: HashMap::new(),
            downloads: HashMap::new(),
        }));
        let shared = Arc::clone(&state);
        let responder = Responder::handle(move |request| {
            let mut state = shared.lock().expect("fake state");
            handle(&mut state, request)
        });
        responder
            .url()
            .clone_into(&mut state.lock().expect("fake state").base);
        Self { responder, state }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }

    /// `http://127.0.0.1:<port>/`.
    pub fn url(&self) -> Url {
        Url::parse(&format!("{}/", self.responder.url())).expect("fake url")
    }

    /// Sign-in endpoints on this server.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::at(&self.url())
    }

    /// A source configuration for the whole fake drive.
    pub fn config(&self) -> ICloudConfig {
        ICloudConfig::with_endpoints(&self.endpoints())
    }

    /// A trusted session, as a completed sign-in leaves it (idmsa session
    /// and trust tokens, the web cookies `accountLogin` sets), issued
    /// directly. Signing in is custody's (`account_custody::icloud::auth`),
    /// whose tests run the full SRP and two-factor flow against this server.
    pub fn session(&self) -> StoredSession {
        let mut state = self.lock();
        let trust = state.next("tt-");
        let token = state.next("st-");
        let web = state.next("wt-");
        let validate = state.next("val-");
        state.trust_tokens.insert(trust.clone());
        state.session_tokens.insert(token.clone(), true);
        state.web_tokens.insert(web.clone());
        let info: AccountInfo =
            serde_json::from_value(auth::account(&state, true)).expect("account description");
        drop(state);
        let cookies = Cookies::from_header(&format!(
            "X-APPLE-WEBAUTH-TOKEN=\"v=2:t={web}\"; X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; \
             X-APPLE-WEBAUTH-VALIDATE=\"v=1:t={validate}\"; X-APPLE-DS-WEB-SESSION-TOKEN=\"fake\""
        ));
        StoredSession::from_login(
            APPLE_ID,
            "USA",
            Zeroizing::new(token),
            Zeroizing::new(trust),
            cookies,
            &info,
        )
        .expect("a trusted session")
    }

    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    pub fn inject(&self, fault: FakeFault) {
        self.lock().faults.push_back(fault);
    }

    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// Every web session cookie stops working; session and trust tokens can
    /// still renew them.
    pub fn expire_web_sessions(&self) {
        self.lock().web_tokens.clear();
    }

    /// Web sessions, idmsa session tokens and trust tokens all lapse: only a
    /// new sign-in with a code helps.
    pub fn expire_trust(&self) {
        let mut state = self.lock();
        state.web_tokens.clear();
        state.session_tokens.clear();
        state.trust_tokens.clear();
    }

    pub fn set_advanced_data_protection(&self, enabled: bool) {
        self.lock().options.advanced_data_protection = enabled;
    }

    /// The bytes of the file at `path` (relative to the drive root), for
    /// assertions behind the client's back.
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        let state = self.lock();
        let doc = drive::lookup(&state, path)?;
        let bytes = state
            .nodes
            .get(&doc)
            .filter(|node| !node.folder)
            .map(|node| node.bytes.clone());
        drop(state);
        bytes
    }

    /// Names of the live children of the folder at `path`.
    pub fn names(&self, path: &str) -> Vec<String> {
        let state = self.lock();
        let Some(doc) = drive::lookup(&state, path) else {
            return Vec::new();
        };
        drive::children(&state, &doc)
            .into_iter()
            .filter_map(|child| state.nodes.get(&child).map(|node| node.name.clone()))
            .collect()
    }
}

fn json_reply(status: u16, value: &serde_json::Value) -> Reply {
    Reply::status(status)
        .header("Content-Type", "application/json")
        .body(value.to_string())
}

fn handle(state: &mut State, request: &Request) -> Option<Reply> {
    if state.offline {
        return None;
    }
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |(path, _)| path)
        .to_owned();
    if path.starts_with("/appleauth/") {
        return Some(auth::idmsa(state, request, &path));
    }
    let content = path.starts_with("/content/");
    if !content && let Some(reply) = take_fault(state) {
        return Some(reply);
    }
    if let Some(rest) = path.strip_prefix("/setup/ws/1/") {
        return Some(auth::setup(state, request, rest));
    }
    if content {
        return Some(drive::content(state, request, &path));
    }
    if let Some(rest) = path.strip_prefix("/drivews/") {
        return Some(drive::drivews(state, request, rest));
    }
    if let Some(rest) = path.strip_prefix("/docws/") {
        return Some(drive::docws(state, request, rest));
    }
    Some(Reply::status(404))
}

fn take_fault(state: &mut State) -> Option<Reply> {
    let position = state
        .faults
        .iter()
        .position(|fault| !matches!(fault, FakeFault::TruncatedBody | FakeFault::ClosedEarly))?;
    match state.faults.remove(position)? {
        FakeFault::RateLimited => Some(Reply::status(429).header("Retry-After", "1")),
        FakeFault::ServerError => Some(Reply::status(503)),
        FakeFault::ExpiredSession => Some(json_reply(
            421,
            &serde_json::json!({"error": "Invalid global session"}),
        )),
        FakeFault::TruncatedBody | FakeFault::ClosedEarly => None,
    }
}

/// The value of cookie `name` in a request.
fn cookie(request: &Request, name: &str) -> Option<String> {
    let header = request.header("cookie")?;
    crate::http::Cookies::from_header(header)
        .get(name)
        .map(ToOwned::to_owned)
}

/// The `t=` token of an Apple `v=…:t=…` cookie value.
fn cookie_token(value: &str) -> Option<String> {
    value
        .trim_matches('"')
        .split(':')
        .find_map(|part| part.strip_prefix("t="))
        .map(ToOwned::to_owned)
}

/// The request carries a live web session cookie.
fn has_web_session(state: &State, request: &Request) -> bool {
    cookie(request, "X-APPLE-WEBAUTH-TOKEN")
        .and_then(|value| cookie_token(&value))
        .is_some_and(|token| state.web_tokens.contains(&token))
}

fn query(request: &Request, name: &str) -> Option<String> {
    let (_, query) = request.target.split_once('?')?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}
