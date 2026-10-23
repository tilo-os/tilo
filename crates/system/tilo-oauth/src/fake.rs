//! A fake authorization server and browser for end-to-end tests (feature
//! `fake`).
//!
//! [`FakeAuthorizationServer`] implements the three endpoints a real
//! provider has, over [`test_http::Responder::handle`]: `GET /authorize`
//! remembers the PKCE challenge, redirect URI and client of a new code and
//! redirects the browser back with it; `POST /token` redeems a code only for
//! the verifier whose S256 matches that challenge, and refreshes only
//! refresh tokens it issued and has not revoked; `POST /revoke` revokes.
//! [`browse`] plays the system browser: it follows the redirects from the
//! authorization URL to the loopback listener.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the fake serves each request under its one state lock, like a serialised server"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use test_http::{Reply, Request, Responder};

use crate::{Provider, Refresh, Revocation};
use tilo_lock::MutexRecover as _;

#[derive(Default)]
struct State {
    next: u64,
    /// code → (challenge, `redirect_uri`, `client_id`)
    codes: BTreeMap<String, (String, String, String)>,
    refresh_tokens: BTreeSet<String>,
    revoked: Vec<String>,
    token_lifetime: u64,
    /// When set, the redirect carries this `state` instead of the real one.
    forged: Option<String>,
    /// Every verifier the token endpoint was offered.
    verifiers: Vec<String>,
    refreshes: usize,
    /// When set, every access token issued is this one (so a provider fake
    /// that accepts one fixed token accepts the tokens issued here).
    access_token: Option<String>,
    /// When set, token requests must carry this `client_secret`.
    client_secret: Option<String>,
    token_gate: Option<Arc<ExchangeGate>>,
}

#[derive(Default)]
struct ExchangeGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

/// Holds a real fixture token exchange until its owner releases it. Dropping
/// the gate releases the handler too, so a failed assertion cannot strand it.
pub struct TokenExchangeGate(Arc<ExchangeGate>);
impl TokenExchangeGate {
    pub fn wait_for_exchange(&self, timeout: Duration) -> bool {
        let state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |state| !state.0)
            .unwrap_or_else(PoisonError::into_inner);
        state.0
    }
    pub fn release(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1 = true;
        self.0.changed.notify_all();
    }
}
impl Drop for TokenExchangeGate {
    fn drop(&mut self) {
        self.release();
    }
}

/// See the module docs.
pub struct FakeAuthorizationServer {
    responder: Responder,
    state: Arc<Mutex<State>>,
}

fn query_of(target: &str) -> BTreeMap<String, String> {
    let query = target.split_once('?').map_or("", |(_, query)| query);
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn json(status: u16, body: &serde_json::Value) -> Reply {
    Reply::status(status)
        .header("Content-Type", "application/json")
        .body(body.to_string())
}

impl Default for FakeAuthorizationServer {
    fn default() -> Self {
        Self::start()
    }
}

impl FakeAuthorizationServer {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(State {
            token_lifetime: 3600,
            ..State::default()
        }));
        let shared = Arc::clone(&state);
        let responder = Responder::handle(move |request| Some(handle(&shared, request)));
        Self { responder, state }
    }

    pub fn hold_token_exchange(&self) -> TokenExchangeGate {
        let gate = Arc::new(ExchangeGate::default());
        self.state.lock_recover().token_gate = Some(gate.clone());
        TokenExchangeGate(gate)
    }

    /// A provider descriptor pointing at this server.
    pub fn provider(&self) -> Provider {
        let base = self.responder.url();
        Provider {
            id: "fake".into(),
            label: "Fake Provider".into(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            revocation: Revocation::Rfc7009 {
                endpoint: format!("{base}/revoke"),
            },
            scopes: vec!["files".into(), "offline".into()],
            authorization_params: vec![("access_type".into(), "offline".into())],
            refresh: Refresh::RefreshToken,
            device_authorization_endpoint: None,
        }
    }

    pub fn url(&self) -> String {
        self.responder.url()
    }

    /// Lifetime of access tokens issued from now on.
    pub fn set_token_lifetime(&self, seconds: u64) {
        self.state.lock_recover().token_lifetime = seconds;
    }

    pub fn issue_access_token(&self, token: &str) {
        self.state.lock_recover().access_token = Some(token.to_string());
    }

    /// Refuse token requests without this `client_secret`, as Google does
    /// for installed-app clients.
    pub fn require_client_secret(&self, secret: &str) {
        self.state.lock_recover().client_secret = Some(secret.to_string());
    }

    /// Accept `token` as a refresh token this server issued, for a grant
    /// sealed before the test started (a vault written by an earlier
    /// version).
    pub fn accept_refresh_token(&self, token: &str) {
        self.state
            .lock_recover()
            .refresh_tokens
            .insert(token.to_string());
    }

    /// Make the redirect carry `state` instead of the request's own.
    pub fn forge_state(&self, state: &str) {
        self.state.lock_recover().forged = Some(state.to_string());
    }

    /// Revoke every refresh token server-side, as a person removing the
    /// app's access in their provider account does.
    pub fn revoke_all_refresh_tokens(&self) {
        let mut state = self.state.lock_recover();
        let tokens: Vec<String> = std::mem::take(&mut state.refresh_tokens)
            .into_iter()
            .collect();
        state.revoked.extend(tokens);
    }

    /// Tokens revoked through `POST /revoke` or [`Self::revoke_all_refresh_tokens`].
    pub fn revoked(&self) -> Vec<String> {
        self.state.lock_recover().revoked.clone()
    }

    /// Verifiers offered to the token endpoint.
    pub fn verifiers(&self) -> Vec<String> {
        self.state.lock_recover().verifiers.clone()
    }

    /// Successful refresh grants served.
    pub fn refreshes(&self) -> usize {
        self.state.lock_recover().refreshes
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<Request> {
        self.responder.drain_requests()
    }
}

fn handle(shared: &Mutex<State>, request: &Request) -> Reply {
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |(path, _)| path);
    if request.method == "POST"
        && path == "/token"
        && let Some(gate) = shared.lock_recover().token_gate.clone()
    {
        let mut state = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = true;
        gate.changed.notify_all();
        drop(
            gate.changed
                .wait_while(state, |state| !state.1)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
    match (request.method.as_str(), path) {
        ("GET", "/authorize") => authorize(shared, &query_of(&request.target)),
        ("POST", "/token") => token(shared, &query_of(&format!("?{}", request.body_text()))),
        ("POST", "/revoke") => {
            let form = query_of(&format!("?{}", request.body_text()));
            let mut state = shared.lock_recover();
            if let Some(token) = form.get("token") {
                state.refresh_tokens.remove(token);
                state.revoked.push(token.clone());
            }
            Reply::status(200)
        }
        _ => Reply::status(404),
    }
}

fn authorize(shared: &Mutex<State>, query: &BTreeMap<String, String>) -> Reply {
    let (Some(challenge), Some(client)) = (query.get("code_challenge"), query.get("client_id"))
    else {
        return Reply::status(400);
    };
    // Without a redirect URI the code is displayed for pasting, the way
    // Dropbox does.
    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
    if query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || query.get("response_type").map(String::as_str) != Some("code")
    {
        return Reply::status(400);
    }
    let mut state = shared.lock_recover();
    state.next = state.next.saturating_add(1);
    let code = format!("code-{}", state.next);
    state.codes.insert(
        code.clone(),
        (challenge.clone(), redirect.clone(), client.clone()),
    );
    if redirect.is_empty() {
        return Reply::status(200)
            .header("Content-Type", "text/plain")
            .body(code);
    }
    let returned_state = state
        .forged
        .clone()
        .or_else(|| query.get("state").cloned())
        .unwrap_or_default();
    let mut location = url::form_urlencoded::Serializer::new(String::new());
    location
        .append_pair("code", &code)
        .append_pair("state", &returned_state);
    Reply::status(302).header("Location", &format!("{redirect}?{}", location.finish()))
}

fn token(shared: &Mutex<State>, form: &BTreeMap<String, String>) -> Reply {
    let invalid_grant = || json(400, &serde_json::json!({"error": "invalid_grant"}));
    let mut state = shared.lock_recover();
    if let Some(secret) = &state.client_secret
        && form.get("client_secret") != Some(secret)
    {
        return json(401, &serde_json::json!({"error": "invalid_client"}));
    }
    let issued = |state: &State| {
        state
            .access_token
            .clone()
            .unwrap_or_else(|| format!("access-{}", state.next))
    };
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            state.verifiers.push(verifier.clone());
            let Some((challenge, redirect, client)) =
                form.get("code").and_then(|code| state.codes.remove(code))
            else {
                return invalid_grant();
            };
            if crate::s256_challenge(&verifier) != challenge
                || form.get("redirect_uri").map_or("", String::as_str) != redirect
                || form.get("client_id") != Some(&client)
            {
                return invalid_grant();
            }
            state.next = state.next.saturating_add(1);
            let access = issued(&state);
            let refresh = format!("refresh-{}", state.next);
            state.refresh_tokens.insert(refresh.clone());
            json(
                200,
                &serde_json::json!({
                    "access_token": access,
                    "refresh_token": refresh,
                    "expires_in": state.token_lifetime,
                    "token_type": "Bearer",
                }),
            )
        }
        Some("refresh_token") => {
            let Some(refresh) = form.get("refresh_token") else {
                return invalid_grant();
            };
            if !state.refresh_tokens.contains(refresh) {
                return invalid_grant();
            }
            state.next = state.next.saturating_add(1);
            state.refreshes = state.refreshes.saturating_add(1);
            json(
                200,
                &serde_json::json!({
                    "access_token": issued(&state),
                    "expires_in": state.token_lifetime,
                    "token_type": "Bearer",
                }),
            )
        }
        _ => json(400, &serde_json::json!({"error": "unsupported_grant_type"})),
    }
}

/// Play the system browser: `GET` `url` and follow `Location` redirects
/// (plain `http` only) until a page answers without one. Returns that page.
///
/// # Errors
/// A description of the connection or protocol failure.
pub fn browse(url: &str) -> Result<String, String> {
    let mut current = url.to_string();
    for _ in 0..5 {
        let (status, location, body) = get(&current)?;
        match location {
            Some(next) if (300..400).contains(&status) => current = next,
            _ => return Ok(body),
        }
    }
    Err("too many redirects".into())
}

fn get(url: &str) -> Result<(u16, Option<String>, String), String> {
    let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() != "http" {
        return Err("the fake browser only speaks http".into());
    }
    let host = parsed.host_str().ok_or("no host")?.to_string();
    let port = parsed.port_or_known_default().ok_or("no port")?;
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let mut stream = TcpStream::connect((host.as_str(), port)).map_err(|e| e.to_string())?;
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("malformed status line")?;
    let location = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_string())
    });
    Ok((status, location, body.to_string()))
}
