//! A stand-in for the model providers Tilo signs in to and asks, on loopback: `OpenAI`'s
//! sign-in the way Codex's `login --device-auth` does it, the `ChatGPT` backend a signed-in
//! plan is asked at, `OpenAI`'s API, and Anthropic's Messages API.
//!
//! Tests start it in-process ([`FakeProviders::start`]) and act as the person and the
//! providers: [`FakeProviders::approve`] enters a device code, and the model APIs answer
//! every request with a scripted turn that names the provider and model it answers as, so a
//! test sees which one answered. A request whose newest user message says `do <tool>
//! <json-args>` (one per line, for tools the request offers) is answered with those calls;
//! a request ending in tool results is answered with their summary. Usage is reported in
//! the counters each API sends: about a quarter of the request's and the answer's bytes.
//!
//! Every request is recorded ([`FakeProviders::requests`]) and the refreshes are counted
//! ([`FakeProviders::refresh_count`]), so a test can assert what was actually asked.
//!
//! `fake-model-providers --addr 127.0.0.1:PORT` serves the same fake for a local stack and
//! prints the four addresses a server is configured with.

#![allow(
    clippy::indexing_slicing,
    reason = "the fake indexes into the JSON objects it built itself"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "the counters are small and bounded, and a wrap would only make a token repeat"
)]

mod auth;
mod models;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use serde_json::Value;

/// The `ChatGPT` account every access token names and the backend routes.
pub const ACCOUNT_ID: &str = "acct-fake";
/// The person every sign-in signs in.
pub const EMAIL: &str = "pat@example.com";
/// The prefix of every API key the sign-in's token exchange issues.
pub const API_KEY_PREFIX: &str = "sk-fake-api-";

/// One request the fake received.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// The path as received, under `/auth`, `/backend-api/codex`, `/openai/v1` or
    /// `/anthropic/v1`.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// The `application/x-www-form-urlencoded` fields, when it carried a form.
    pub form: Vec<(String, String)>,
    /// The JSON body, when it carried one.
    pub json: Value,
    /// The headers a test asserts on: `authorization`, `chatgpt-account-id`, `x-api-key`
    /// and `content-type`, lowercased.
    pub headers: Vec<(String, String)>,
    /// The body's length in bytes, which the reported input tokens follow.
    pub bytes: usize,
}

impl Recorded {
    /// A header of the request, by its lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// A field of the request's form or JSON body, whichever it carried.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.form
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .or_else(|| self.json.get(name).and_then(Value::as_str))
    }
}

/// One sign-in's one-time code.
#[derive(Clone, Debug)]
struct DeviceAuth {
    id: String,
    user_code: String,
    approved: bool,
    /// What the polling was handed, once the code was entered.
    granted: Option<Granted>,
}

#[derive(Clone, Debug)]
struct Granted {
    authorization_code: String,
    code_verifier: String,
    code_challenge: String,
}

#[derive(Debug)]
struct State {
    device_auths: Vec<DeviceAuth>,
    /// An authorization code and the PKCE verifier it was issued with.
    codes: HashMap<String, String>,
    /// A live access token and when it expires (seconds since the epoch).
    access: HashMap<String, u64>,
    refresh: HashSet<String>,
    /// Refresh tokens already spent, which a reuse is refused.
    used_refresh: HashSet<String>,
    api_keys: HashSet<String>,
    /// What a sign-in and a refresh grant, in seconds.
    expires_in: u64,
    /// Whether the token exchange refuses to issue an API key.
    refuse_api_key: bool,
    refresh_count: u32,
    requests: Vec<Recorded>,
    /// `http://127.0.0.1:PORT`.
    base: String,
    next: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            device_auths: Vec::new(),
            codes: HashMap::new(),
            access: HashMap::new(),
            refresh: HashSet::new(),
            used_refresh: HashSet::new(),
            api_keys: HashSet::new(),
            expires_in: 3600,
            refuse_api_key: false,
            refresh_count: 0,
            requests: Vec::new(),
            base: String::new(),
            next: 0,
        }
    }
}

impl State {
    /// The next number a token is built from.
    fn mint(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    fn issuer(&self) -> String {
        format!("{}/auth", self.base)
    }
}

struct Shared {
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The fake, serving on loopback until dropped.
pub struct FakeProviders {
    shared: Arc<Shared>,
    addr: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FakeProviders {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FakeProviders {
    /// Start the fake on a port of the loopback interface.
    pub async fn start() -> std::io::Result<Self> {
        Self::start_on(([127, 0, 0, 1], 0).into()).await
    }

    /// Start the fake on `addr`, which must be loopback.
    pub async fn start_on(addr: SocketAddr) -> std::io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(std::io::Error::other("the fake only listens on loopback"));
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                base: format!("http://{addr}"),
                ..State::default()
            }),
        });
        let tasks = vec![tokio::spawn(serve(Arc::clone(&shared), listener))];
        Ok(Self {
            shared,
            addr,
            tasks,
        })
    }

    /// The fake's address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:PORT`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `{base}/auth`, the issuer a server signs people in at.
    #[must_use]
    pub fn issuer_url(&self) -> String {
        format!("{}/auth", self.base_url())
    }

    /// `{base}/backend-api/codex`, a signed-in plan's backend.
    #[must_use]
    pub fn chatgpt_backend_url(&self) -> String {
        format!("{}/backend-api/codex", self.base_url())
    }

    /// `{base}/openai/v1`, `OpenAI`'s API with a key.
    #[must_use]
    pub fn openai_api_url(&self) -> String {
        format!("{}/openai/v1", self.base_url())
    }

    /// `{base}/anthropic/v1`, an Anthropic endpoint.
    #[must_use]
    pub fn anthropic_api_url(&self) -> String {
        format!("{}/anthropic/v1", self.base_url())
    }

    /// Enter `user_code` as the person would: the sign-in's next poll is handed its
    /// grant. False when no sign-in has that code.
    pub fn approve(&self, user_code: &str) -> bool {
        let mut state = self.shared.lock();
        match state
            .device_auths
            .iter_mut()
            .find(|auth| auth.user_code.eq_ignore_ascii_case(user_code.trim()))
        {
            Some(auth) => {
                auth.approved = true;
                true
            }
            None => false,
        }
    }

    /// The codes of the sign-ins nobody entered, oldest first.
    #[must_use]
    pub fn pending_codes(&self) -> Vec<String> {
        self.shared
            .lock()
            .device_auths
            .iter()
            .filter(|auth| !auth.approved)
            .map(|auth| auth.user_code.clone())
            .collect()
    }

    /// What a sign-in and a refresh grant, in seconds (an hour by default). Zero grants a
    /// token that has already expired.
    pub fn set_token_lifetime(&self, seconds: u64) {
        self.shared.lock().expires_in = seconds;
    }

    /// Whether the token exchange issues API keys; false makes it refuse, as for an
    /// account with no API organisation.
    pub fn set_api_key_exchange(&self, allowed: bool) {
        self.shared.lock().refuse_api_key = !allowed;
    }

    /// How many refreshes the token endpoint granted.
    #[must_use]
    pub fn refresh_count(&self) -> u32 {
        self.shared.lock().refresh_count
    }

    /// Every request the fake received, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.lock().requests.clone()
    }
}

async fn serve(shared: Arc<Shared>, listener: tokio::net::TcpListener) {
    while let Ok((socket, _)) = listener.accept().await {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let shared = Arc::clone(&shared);
                async move { Ok::<_, Infallible>(handle(&shared, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                .await;
        });
    }
}

/// One HTTP answer.
pub(crate) struct Answer {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
}

impl Answer {
    pub(crate) fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            body: value.to_string().into_bytes(),
            content_type: "application/json",
            headers: Vec::new(),
        }
    }

    pub(crate) fn html(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.as_bytes().to_vec(),
            content_type: "text/html; charset=utf-8",
            headers: Vec::new(),
        }
    }

    pub(crate) fn sse(body: String) -> Self {
        Self {
            status: 200,
            body: body.into_bytes(),
            content_type: "text/event-stream",
            headers: vec![("cache-control", "no-store".into())],
        }
    }

    fn not_found() -> Self {
        Self::json(404, &serde_json::json!({"error": "not found"}))
    }
}

async fn handle(shared: &Shared, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query: Vec<(String, String)> = request
        .uri()
        .query()
        .map(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| (decode(key), decode(value)))
                .collect()
        })
        .unwrap_or_default();
    let headers: Vec<(String, String)> = ["authorization", "chatgpt-account-id", "x-api-key"]
        .into_iter()
        .chain(["content-type"])
        .filter_map(|name| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (name.to_owned(), value.to_owned()))
        })
        .collect();
    let body = request
        .into_body()
        .collect()
        .await
        .map(|body| body.to_bytes().to_vec())
        .unwrap_or_default();
    let content_type = headers
        .iter()
        .find(|(name, _)| name == "content-type")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let recorded = Recorded {
        method,
        path: path.clone(),
        query,
        form: form_of(&content_type, &body),
        json: json_of(&content_type, &body),
        headers,
        bytes: body.len(),
    };
    let mut state = shared.lock();
    state.requests.push(recorded.clone());
    let answer = route(&mut state, &recorded);
    drop(state);
    let mut response = Response::builder()
        .status(answer.status)
        .header("content-type", answer.content_type);
    for (name, value) in answer.headers {
        response = response.header(name, value);
    }
    response
        .body(Full::new(Bytes::from(answer.body)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn route(state: &mut State, request: &Recorded) -> Answer {
    if let Some(rest) = request.path.strip_prefix("/auth") {
        return auth::route(state, request, rest);
    }
    for (prefix, kind) in [
        ("/backend-api/codex", models::Kind::ChatGpt),
        ("/openai/v1", models::Kind::OpenAi),
        ("/anthropic/v1", models::Kind::Anthropic),
    ] {
        if let Some(rest) = request.path.strip_prefix(prefix) {
            return models::route(state, request, kind, rest);
        }
    }
    Answer::not_found()
}

/// The JSON a request carried, when its content type says so.
fn json_of(content_type: &str, body: &[u8]) -> Value {
    if content_type.contains("json") {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

/// The form fields a request carried, when its content type says so.
fn form_of(content_type: &str, body: &[u8]) -> Vec<(String, String)> {
    if !content_type.contains("form") {
        return Vec::new();
    }
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (decode(key), decode(value)))
        .collect()
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// Seconds since the epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// An unsigned JWT: a header, `claims`, and a signature nobody verifies.
fn jwt(claims: &Value) -> String {
    use base64::Engine as _;
    use serde_json::json;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = engine.encode(json!({"alg": "RS256", "typ": "JWT"}).to_string());
    let payload = engine.encode(claims.to_string());
    let signature = engine.encode(b"fake-signature");
    format!("{header}.{payload}.{signature}")
}

/// The claims of a JWT, when it is one.
fn jwt_claims(token: &str) -> Option<Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Tokens minted as `expires_in` says; both names of a token are kept live.
fn tokens(state: &mut State) -> Value {
    use serde_json::json;
    let now = unix_now();
    let expires_in = state.expires_in;
    let expires = now.saturating_add(expires_in);
    let id = state.mint();
    let auth = json!({
        "chatgpt_account_id": ACCOUNT_ID,
        "chatgpt_plan_type": "plus",
    });
    let claims = |extra: Value| {
        let mut claims = json!({
            "iss": state.issuer(),
            "sub": "user-fake",
            "email": EMAIL,
            "iat": now,
            "exp": expires,
            "https://api.openai.com/auth": auth,
        });
        if let (Some(claims), Some(extra)) = (claims.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                claims.insert(key.clone(), value.clone());
            }
        }
        claims
    };
    let access_token = jwt(&claims(json!({"sid": format!("access-{id}")})));
    let id_token = jwt(&claims(json!({"sid": format!("id-{id}")})));
    state.access.insert(access_token.clone(), expires);
    let refresh_token = format!("rt-{id:016x}");
    state.refresh.insert(refresh_token.clone());
    json!({
        "id_token": id_token,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_in": expires_in,
        "token_type": "Bearer",
        "scope": "openid profile email",
    })
}

/// The bearer credential of an `Authorization` header.
fn bearer(request: &Recorded) -> Option<&str> {
    request.header("authorization")?.strip_prefix("Bearer ")
}

/// Whether an access token is one this fake issued and has not expired.
fn live_access(state: &State, token: &str) -> bool {
    state
        .access
        .get(token)
        .is_some_and(|expires| *expires > unix_now())
}
