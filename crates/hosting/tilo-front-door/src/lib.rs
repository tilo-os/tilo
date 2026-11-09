//! The front door of hosted Tilo for agents: one MCP endpoint an agent
//! connects to with nothing configured but its URL, as in
//!
//! ```text
//! claude mcp add --transport http tilo https://<door>
//! ```
//!
//! The agent finds out how to authorize from the door itself (the MCP
//! authorization profile: OAuth 2.1, [`oauth`]); the person signs in with a
//! device of theirs ([`sign_in`]) and chooses what the agent may reach:
//!
//! - **All my workspaces**: the agent lists and creates workspaces, and
//!   names the workspace each time it acts;
//! - **One workspace**: the agent is pinned to it and cannot see or reach
//!   another. Pointing the agent at `<door>/w/<workspace>` asks for exactly
//!   that.
//!
//! The door is the hosting provider's proxy that `tilo-runtime`'s HTTP
//! endpoint expects in front of it: it owns the person's authentication,
//! routes each session to the endpoint of the workspace's host
//! ([`directory`]), and presents that endpoint's token. Every MCP session is
//! one agent run, attributed separately in History and named for the person
//! ("Claude Code #1", [`session`]). The person revokes a grant at
//! `/grants`; its tokens and sessions end at once.

pub mod directory;
pub mod hosts;
pub mod oauth;
pub mod pages;
pub mod session;
pub mod sign_in;
pub mod store;

use directory::Directory;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{self, HeaderValue},
};
use serde_json::{Value, json};
use session::{SESSION_HEADER, Session};
use std::{
    collections::HashMap,
    fmt::Write as _,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use store::{Scope, Store};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 4_096;
const MAX_SESSIONS_PER_GRANT: usize = 64;
const SESSION_IDLE: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_PENDING: usize = 4_096;
/// How long a person has to sign in and decide.
const PENDING_LIFETIME_MS: u64 = 15 * 60 * 1000;

/// What the door is called and where people sign in.
#[derive(Clone, Debug)]
pub struct Config {
    /// The door's own origin, as agents reach it (`https://mcp.example`): the
    /// OAuth issuer and the protected resource. No trailing slash.
    pub issuer: String,
    /// The person's Tilo on the web, which signs them in to the door.
    pub web_entry: String,
}

/// An authorization in progress: the agent's request, waiting for the person.
#[derive(Clone)]
struct Pending {
    request: oauth::AuthorizeRequest,
    client_name: String,
    challenge: [u8; 32],
    /// Who signed in for it, once someone has.
    person: Option<String>,
    /// The token the consent form carries, so only the door's own page
    /// can submit the decision.
    csrf: String,
    expires_ms: u64,
}

/// The front door.
pub struct Door {
    config: Config,
    /// `<web_entry>/sign-in`, where the person's Tilo signs them in.
    sign_in_entry: url::Url,
    store: Store,
    directory: Arc<dyn Directory>,
    browsers: sign_in::Browsers,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Per signed-in browser, the token its grants page forms carry.
    page_tokens: Mutex<HashMap<String, String>>,
    http: tilo_net::Client,
}

type Reply = Response<Full<Bytes>>;

impl Door {
    /// # Errors
    /// The web entry is not a URL, or the HTTP client for workspace
    /// endpoints could not be built.
    pub fn new(
        config: &Config,
        store: Store,
        directory: Arc<dyn Directory>,
    ) -> io::Result<Arc<Self>> {
        let http = tilo_net::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirects(tilo_net::Redirects::None)
            .build()
            .map_err(io::Error::other)?;
        let web_entry = config.web_entry.trim_end_matches('/').to_owned();
        let sign_in_entry = url::Url::parse(&format!("{web_entry}/sign-in"))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Ok(Arc::new(Self {
            config: Config {
                issuer: config.issuer.trim_end_matches('/').to_owned(),
                web_entry,
            },
            sign_in_entry,
            store,
            directory,
            browsers: sign_in::Browsers::default(),
            pending: Mutex::default(),
            sessions: Mutex::default(),
            page_tokens: Mutex::default(),
            http,
        }))
    }

    /// The live MCP sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// Answer one HTTP request.
    pub async fn respond(self: &Arc<Self>, request: Request<Incoming>) -> Reply {
        let (parts, body) = request.into_parts();
        let body = match Limited::new(body, MAX_BODY_BYTES).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => return text(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 1 MiB"),
        };
        self.dispatch(
            &parts.method,
            parts.uri.path(),
            parts.uri.query().unwrap_or(""),
            &parts.headers,
            &body,
        )
        .await
    }

    /// Routing, separated from hyper's body type for tests.
    pub async fn dispatch(
        self: &Arc<Self>,
        method: &Method,
        path: &str,
        query: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let issuer = self.config.issuer.clone();
        match (method, path) {
            (&Method::GET, "/health") => json_reply(StatusCode::OK, &json!({"ok":true})),
            (&Method::GET, oauth::AUTHORIZATION_SERVER_PATH | oauth::OPENID_CONFIGURATION_PATH) => {
                json_reply(StatusCode::OK, &oauth::server_metadata(&issuer))
            }
            (&Method::GET, path)
                if let Some(rest) = path.strip_prefix(oauth::PROTECTED_RESOURCE_PATH) =>
            {
                match resource_of(rest) {
                    Some(pinned) => json_reply(
                        StatusCode::OK,
                        &oauth::resource_metadata(&issuer, &self.resource_url(pinned.as_deref())),
                    ),
                    None => text(StatusCode::NOT_FOUND, "not found"),
                }
            }
            (&Method::POST, oauth::REGISTER_PATH) => self.register(body),
            (&Method::GET, oauth::AUTHORIZE_PATH) => self.authorize(query, headers),
            (&Method::POST, "/sign-in") => self.signed_in(body).await,
            (&Method::GET, path) if let Some(id) = path.strip_prefix("/consent/") => {
                self.consent_page(id, headers).await
            }
            (&Method::POST, path) if let Some(id) = path.strip_prefix("/consent/") => {
                self.decide(id, headers, body).await
            }
            (&Method::POST, oauth::TOKEN_PATH) => self.token(body),
            (&Method::POST, oauth::REVOKE_PATH) => self.revoke(body).await,
            (&Method::GET, "/grants") => self.grants_page(headers).await,
            (&Method::POST, path)
                if let Some(grant) = path
                    .strip_prefix("/grants/")
                    .and_then(|rest| rest.strip_suffix("/revoke")) =>
            {
                self.revoke_from_page(grant, headers, body).await
            }
            (_, path) => match resource_of(path) {
                Some(pinned) => self.mcp(method, pinned.as_deref(), headers, body).await,
                None => text(StatusCode::NOT_FOUND, "not found"),
            },
        }
    }

    /// The resource URL agents use: the door, or one workspace of it.
    fn resource_url(&self, pinned: Option<&str>) -> String {
        match pinned {
            Some(workspace) => format!("{}/w/{workspace}", self.config.issuer),
            None => self.config.issuer.clone(),
        }
    }

    fn register(&self, body: &[u8]) -> Reply {
        let Ok(request) = serde_json::from_slice::<Value>(body) else {
            return json_reply(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid_client_metadata","error_description":"the body must be JSON"}),
            );
        };
        match oauth::registration(&request) {
            Ok((name, uris)) => match self.store.register(name, uris, now_ms()) {
                Ok(client) => json_reply(StatusCode::CREATED, &oauth::registered(&client)),
                Err(_) => text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "registration is unavailable",
                ),
            },
            Err((error, description)) => json_reply(
                StatusCode::BAD_REQUEST,
                &json!({"error":error,"error_description":description}),
            ),
        }
    }

    fn authorize(&self, query: &str, headers: &hyper::HeaderMap) -> Reply {
        let query = oauth::form(query.as_bytes()).unwrap_or_default();
        let client = query.get("client_id").and_then(|id| self.store.client(id));
        let request = match oauth::authorize_request(&query, client.as_ref(), &self.config.issuer) {
            Ok(request) => request,
            Err(oauth::AuthorizeError::Untrusted(why)) => {
                return html(
                    StatusCode::BAD_REQUEST,
                    &pages::message("This app cannot connect", why),
                );
            }
            Err(oauth::AuthorizeError::Redirect {
                redirect_uri,
                state,
                error,
                description,
            }) => {
                let mut params = vec![("error", error), ("error_description", description)];
                if let Some(state) = state.as_deref() {
                    params.push(("state", state));
                }
                return redirect_back(&redirect_uri, &params);
            }
        };
        let now = now_ms();
        let mut challenge = [0u8; 32];
        let (Ok(()), Ok(id), Ok(csrf)) = (
            getrandom::fill(&mut challenge),
            store::secret(24),
            store::secret(24),
        ) else {
            return no_randomness();
        };
        let person = self.browsers.person(headers, now);
        let signed_in = person.is_some();
        {
            let mut pending = lock(&self.pending);
            pending.retain(|_, pending| pending.expires_ms > now);
            if pending.len() >= MAX_PENDING {
                return text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many sign-ins in progress",
                );
            }
            pending.insert(
                id.clone(),
                Pending {
                    request,
                    client_name: client.map(|client| client.name).unwrap_or_default(),
                    challenge,
                    person,
                    csrf,
                    expires_ms: now.saturating_add(PENDING_LIFETIME_MS),
                },
            );
        }
        if signed_in {
            return redirect(&format!("/consent/{id}"));
        }
        let client_name = lock(&self.pending)
            .get(&id)
            .map(|pending| pending.client_name.clone())
            .unwrap_or_default();
        html(
            StatusCode::OK,
            &pages::sign_in(&client_name, &self.sign_in_url(&id, &challenge)),
        )
    }

    /// Where the person's Tilo signs them in to this door, for `pending`.
    fn sign_in_url(&self, pending: &str, challenge: &[u8; 32]) -> String {
        let mut url = self.sign_in_entry.clone();
        url.query_pairs_mut()
            .append_pair("audience", &self.config.issuer)
            .append_pair("challenge", &hex::encode(challenge))
            .append_pair("return", &format!("{}/sign-in", self.config.issuer))
            .append_pair("pending", pending);
        url.into()
    }

    /// `POST /sign-in`: the person's Tilo answers a pending sign-in with a
    /// proof.
    async fn signed_in(&self, body: &[u8]) -> Reply {
        let form = oauth::form(body).unwrap_or_default();
        let (Some(id), Some(proof)) = (
            form.get("pending"),
            form.get("proof")
                .and_then(|proof| sign_in::decode_proof(proof)),
        ) else {
            return html(
                StatusCode::BAD_REQUEST,
                &pages::message("Sign-in failed", "The sign-in was incomplete."),
            );
        };
        let now = now_ms();
        let mut fresh = [0u8; 32];
        if getrandom::fill(&mut fresh).is_err() {
            return no_randomness();
        }
        // The challenge is spent by this attempt, whatever its outcome.
        let challenge = {
            let mut pending = lock(&self.pending);
            match pending
                .get_mut(id)
                .filter(|pending| pending.expires_ms > now && pending.person.is_none())
            {
                Some(found) => std::mem::replace(&mut found.challenge, fresh),
                None => {
                    return html(
                        StatusCode::BAD_REQUEST,
                        &pages::message("Sign-in expired", "Start connecting the agent again."),
                    );
                }
            }
        };
        let Ok(revocations) = self
            .directory
            .revocations(&proof.cert.user_public_key)
            .await
        else {
            return html(
                StatusCode::SERVICE_UNAVAILABLE,
                &pages::message(
                    "Sign-in unavailable",
                    "Your devices could not be checked. Try again shortly.",
                ),
            );
        };
        let person = match sign_in::verify(
            &proof,
            &self.config.issuer,
            &challenge,
            &revocations,
            now.saturating_mul(1_000_000),
        ) {
            Ok(person) => person,
            Err(error) => {
                tracing::info!(%error, "sign-in refused");
                return html(
                    StatusCode::FORBIDDEN,
                    &pages::message("Sign-in refused", &error.to_string()),
                );
            }
        };
        if let Some(found) = lock(&self.pending).get_mut(id) {
            found.person = Some(person.clone());
        }
        let Ok(cookie) = self.browsers.sign_in(person, now) else {
            return no_randomness();
        };
        let mut response = redirect(&format!("/consent/{id}"));
        if let Ok(value) = HeaderValue::from_str(&sign_in::set_cookie(&cookie)) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        response
    }

    /// The pending authorization `id`, for the person signed in at this
    /// browser, who must be the one who signed in for it.
    fn pending_for(&self, id: &str, headers: &hyper::HeaderMap) -> Option<(Pending, String)> {
        let now = now_ms();
        let person = self.browsers.person(headers, now)?;
        let pending = lock(&self.pending)
            .get(id)
            .filter(|pending| pending.expires_ms > now)
            .cloned()?;
        match &pending.person {
            Some(signed) if *signed == person => Some((pending, person)),
            // Signed in before the agent asked: this browser's person decides.
            None => {
                if let Some(found) = lock(&self.pending).get_mut(id) {
                    found.person = Some(person.clone());
                }
                Some((pending, person))
            }
            Some(_) => None,
        }
    }

    async fn consent_page(&self, id: &str, headers: &hyper::HeaderMap) -> Reply {
        let Some((pending, person)) = self.pending_for(id, headers) else {
            return html(
                StatusCode::FORBIDDEN,
                &pages::message("Sign-in expired", "Start connecting the agent again."),
            );
        };
        let workspaces = match self.directory.workspaces(&person).await {
            Ok(workspaces) => workspaces,
            Err(error) => {
                return html(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &pages::message("Unavailable", &error.to_string()),
                );
            }
        };
        let pinned = match &pending.request.pinned {
            Some(workspace) => match workspaces
                .iter()
                .find(|candidate| &candidate.id == workspace)
            {
                Some(found) => Some(found.clone()),
                None => {
                    return html(
                        StatusCode::FORBIDDEN,
                        &pages::message(
                            "No such workspace",
                            "You have no workspace at the address this agent was given.",
                        ),
                    );
                }
            },
            None => None,
        };
        html(
            StatusCode::OK,
            &pages::consent(
                &pending.client_name,
                &format!("/consent/{id}"),
                &pending.csrf,
                &workspaces,
                pinned.as_ref(),
            ),
        )
    }

    /// `POST /consent/<id>`: the person's decision.
    async fn decide(&self, id: &str, headers: &hyper::HeaderMap, body: &[u8]) -> Reply {
        let Some((pending, person)) = self.pending_for(id, headers) else {
            return html(
                StatusCode::FORBIDDEN,
                &pages::message("Sign-in expired", "Start connecting the agent again."),
            );
        };
        let form = oauth::form(body).unwrap_or_default();
        if !form
            .get("csrf")
            .is_some_and(|csrf| constant_time_eq(csrf.as_bytes(), pending.csrf.as_bytes()))
        {
            return html(
                StatusCode::FORBIDDEN,
                &pages::message("Not allowed", "This decision did not come from this page."),
            );
        }
        // Decided once: a second submission finds nothing.
        lock(&self.pending).remove(id);
        let request = &pending.request;
        let back = |params: &[(&str, &str)]| {
            let mut params = params.to_vec();
            if let Some(state) = request.state.as_deref() {
                params.push(("state", state));
            }
            redirect_back(&request.redirect_uri, &params)
        };
        if form.get("decision").map(String::as_str) != Some("allow") {
            return back(&[("error", "access_denied")]);
        }
        let scope = match (form.get("scope").map(String::as_str), &request.pinned) {
            (Some("workspace"), pinned) => {
                let Some(workspace) = form.get("workspace") else {
                    return back(&[("error", "invalid_request")]);
                };
                if pinned.as_ref().is_some_and(|pinned| pinned != workspace) {
                    return back(&[("error", "invalid_target")]);
                }
                Scope::Workspace(workspace.clone())
            }
            (Some("account"), None) => Scope::Account,
            _ => return back(&[("error", "invalid_request")]),
        };
        if let Scope::Workspace(workspace) = &scope {
            let has = self
                .directory
                .workspaces(&person)
                .await
                .is_ok_and(|workspaces| {
                    workspaces
                        .iter()
                        .any(|candidate| &candidate.id == workspace)
                });
            if !has {
                return back(&[("error", "invalid_target")]);
            }
        }
        let Some(client) = self.store.client(&request.client_id) else {
            return back(&[("error", "unauthorized_client")]);
        };
        match self.store.consent(
            &person,
            &client,
            scope,
            request.redirect_uri.clone(),
            request.challenge.clone(),
            now_ms(),
        ) {
            Ok(code) => back(&[("code", &code)]),
            Err(_) => back(&[("error", "temporarily_unavailable")]),
        }
    }

    fn token(&self, body: &[u8]) -> Reply {
        let Some(form) = oauth::form(body) else {
            return json_reply(StatusCode::BAD_REQUEST, &json!({"error":"invalid_request"}));
        };
        let field = |name: &str| form.get(name).map(String::as_str);
        let now = now_ms();
        let issued = match field("grant_type") {
            Some("authorization_code") => {
                let (Some(code), Some(verifier), Some(client_id)) =
                    (field("code"), field("code_verifier"), field("client_id"))
                else {
                    return json_reply(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":"invalid_request"}),
                    );
                };
                let Some(code) = self.store.take_code(code, now) else {
                    return json_reply(StatusCode::BAD_REQUEST, &json!({"error":"invalid_grant"}));
                };
                let redirect_matches =
                    field("redirect_uri").is_none_or(|uri| uri == code.redirect_uri);
                if code.client_id != client_id
                    || !redirect_matches
                    || !oauth::verifier_valid(verifier)
                    || !constant_time_eq(
                        store::challenge_of(verifier).as_bytes(),
                        code.challenge.as_bytes(),
                    )
                {
                    return json_reply(StatusCode::BAD_REQUEST, &json!({"error":"invalid_grant"}));
                }
                self.store.redeem(code.grant, now)
            }
            Some("refresh_token") => {
                let (Some(refresh), Some(client_id)) = (field("refresh_token"), field("client_id"))
                else {
                    return json_reply(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":"invalid_request"}),
                    );
                };
                self.store.refresh(refresh, client_id, now)
            }
            _ => {
                return json_reply(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"unsupported_grant_type"}),
                );
            }
        };
        match issued {
            Ok(Ok(issued)) => {
                let mut reply = json_reply(
                    StatusCode::OK,
                    &json!({
                        "access_token": issued.access,
                        "token_type": "Bearer",
                        "expires_in": store::ACCESS_LIFETIME.as_secs(),
                        "refresh_token": issued.refresh,
                        "scope": oauth::MCP_SCOPE,
                    }),
                );
                reply
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                reply
            }
            Ok(Err(error)) => json_reply(StatusCode::BAD_REQUEST, &oauth::token_error(&error)),
            Err(_) => text(StatusCode::SERVICE_UNAVAILABLE, "tokens are unavailable"),
        }
    }

    /// `POST /revoke` (RFC 7009): always 200, whatever the token was.
    async fn revoke(&self, body: &[u8]) -> Reply {
        let form = oauth::form(body).unwrap_or_default();
        if let (Some(token), Some(client)) = (form.get("token"), form.get("client_id"))
            && let Ok(Some(grant)) = self.store.revoke_token(token, client, now_ms())
        {
            self.end_sessions_of(&grant).await;
        }
        text(StatusCode::OK, "")
    }

    async fn grants_page(&self, headers: &hyper::HeaderMap) -> Reply {
        let Some(person) = self.browsers.person(headers, now_ms()) else {
            return html(
                StatusCode::UNAUTHORIZED,
                &pages::message(
                    "Sign in first",
                    "Connect an agent to sign in, then manage it here.",
                ),
            );
        };
        let workspaces = self.directory.workspaces(&person).await.unwrap_or_default();
        let Ok(csrf) = self.page_token(headers) else {
            return no_randomness();
        };
        html(
            StatusCode::OK,
            &pages::grants(&self.store.grants_of(&person), &workspaces, &csrf),
        )
    }

    /// The token this browser's grants page forms carry.
    ///
    /// # Errors
    /// The system's random source failed to mint a new one.
    fn page_token(&self, headers: &hyper::HeaderMap) -> io::Result<String> {
        let cookie = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut tokens = lock(&self.page_tokens);
        if let Some(token) = tokens.get(&cookie) {
            return Ok(token.clone());
        }
        let token = store::secret(24)?;
        tokens.insert(cookie, token.clone());
        drop(tokens);
        Ok(token)
    }

    async fn revoke_from_page(
        &self,
        grant: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let Some(person) = self.browsers.person(headers, now_ms()) else {
            return html(
                StatusCode::UNAUTHORIZED,
                &pages::message("Sign in first", "Your sign-in expired."),
            );
        };
        let form = oauth::form(body).unwrap_or_default();
        let Ok(expected) = self.page_token(headers) else {
            return no_randomness();
        };
        if !form
            .get("csrf")
            .is_some_and(|csrf| constant_time_eq(csrf.as_bytes(), expected.as_bytes()))
        {
            return html(
                StatusCode::FORBIDDEN,
                &pages::message("Not allowed", "This did not come from your grants page."),
            );
        }
        match self.revoke_grant(&person, grant).await {
            Ok(_) => redirect("/grants"),
            Err(_) => text(StatusCode::SERVICE_UNAVAILABLE, "revocation is unavailable"),
        }
    }

    /// Revoke `person`'s grant `grant`: its tokens stop working and its
    /// sessions end now.
    ///
    /// # Errors
    /// The store could not be written.
    pub async fn revoke_grant(&self, person: &str, grant: &str) -> io::Result<bool> {
        let revoked = self.store.revoke_grant(person, grant, now_ms())?;
        if revoked {
            self.end_sessions_of(grant).await;
        }
        Ok(revoked)
    }

    async fn end_sessions_of(&self, grant: &str) {
        let ended: Vec<Arc<Session>> = lock(&self.sessions)
            .extract_if(|_, session| session.grant == grant)
            .map(|(_, session)| session)
            .collect();
        for session in ended {
            session.close().await;
        }
    }

    /// The MCP endpoint, at the door's root or pinned to `/w/<workspace>`.
    async fn mcp(
        self: &Arc<Self>,
        method: &Method,
        pinned: Option<&str>,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let presented = bearer(headers);
        let grant = presented.and_then(|token| self.store.access(token, now_ms()));
        let Some(grant) = grant else {
            return self.challenge(pinned, presented.is_some());
        };
        if !oauth::scope_reaches(&grant.scope, pinned) {
            return self.insufficient(pinned);
        }
        match *method {
            Method::POST => self.post(grant, headers, body).await,
            Method::DELETE => match session_id(headers) {
                Some(id) => {
                    let removed = {
                        let mut sessions = lock(&self.sessions);
                        match sessions.get(&id) {
                            Some(session) if session.grant == grant.id => sessions.remove(&id),
                            _ => None,
                        }
                    };
                    match removed {
                        Some(session) => {
                            session.close().await;
                            text(StatusCode::NO_CONTENT, "")
                        }
                        None => text(StatusCode::NOT_FOUND, "unknown session"),
                    }
                }
                None => text(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
            },
            _ => {
                let mut reply = text(StatusCode::METHOD_NOT_ALLOWED, "POST or DELETE");
                reply
                    .headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
                reply
            }
        }
    }

    /// 401 naming where the agent learns how to get a token (RFC 9728).
    fn challenge(&self, pinned: Option<&str>, presented: bool) -> Reply {
        let metadata = format!(
            "{}{}{}",
            self.config.issuer,
            oauth::PROTECTED_RESOURCE_PATH,
            pinned
                .map(|workspace| format!("/w/{workspace}"))
                .unwrap_or_default()
        );
        let mut value = format!(
            "Bearer resource_metadata=\"{metadata}\", scope=\"{}\"",
            oauth::MCP_SCOPE
        );
        if presented {
            value.push_str(", error=\"invalid_token\"");
        }
        let mut reply = text(StatusCode::UNAUTHORIZED, "sign in to use Tilo");
        if let Ok(value) = HeaderValue::from_str(&value) {
            reply.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
        reply
    }

    /// 403: the token is good, but not for this workspace.
    fn insufficient(&self, pinned: Option<&str>) -> Reply {
        let metadata = format!(
            "{}{}{}",
            self.config.issuer,
            oauth::PROTECTED_RESOURCE_PATH,
            pinned
                .map(|workspace| format!("/w/{workspace}"))
                .unwrap_or_default()
        );
        let mut reply = text(
            StatusCode::FORBIDDEN,
            "this grant does not reach that workspace",
        );
        if let Ok(value) = HeaderValue::from_str(&format!(
            "Bearer error=\"insufficient_scope\", resource_metadata=\"{metadata}\", scope=\"{}\"",
            oauth::MCP_SCOPE
        )) {
            reply.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
        reply
    }

    async fn post(
        self: &Arc<Self>,
        grant: store::Grant,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let Ok(request) = serde_json::from_slice::<Value>(body) else {
            return json_reply(
                StatusCode::BAD_REQUEST,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
            );
        };
        if !request.is_object() {
            return json_reply(
                StatusCode::BAD_REQUEST,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"one message per request"}}),
            );
        }
        let initialize = request.get("method").and_then(Value::as_str) == Some("initialize");
        match (session_id(headers), initialize) {
            (None, true) => self.open(grant, &request).await,
            (Some(id), _) => {
                let session = lock(&self.sessions)
                    .get(&id)
                    .filter(|session| session.grant == grant.id)
                    .cloned();
                let Some(session) = session else {
                    return text(StatusCode::NOT_FOUND, "unknown session; initialize again");
                };
                let reply = session.handle(request).await;
                session_reply(&reply, headers, None)
            }
            (None, false) => text(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
        }
    }

    /// `initialize`: a new agent run under `grant`.
    async fn open(self: &Arc<Self>, grant: store::Grant, request: &Value) -> Reply {
        {
            let now = Instant::now();
            let mut sessions = lock(&self.sessions);
            sessions
                .retain(|_, session| now.duration_since(*lock(&session.last_used)) < SESSION_IDLE);
            let of_grant = sessions
                .values()
                .filter(|session| session.grant == grant.id)
                .count();
            if sessions.len() >= MAX_SESSIONS || of_grant >= MAX_SESSIONS_PER_GRANT {
                return text(StatusCode::SERVICE_UNAVAILABLE, "too many agent sessions");
            }
        }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let Ok(number) = self.store.next_run(&grant.person) else {
            return text(StatusCode::SERVICE_UNAVAILABLE, "sessions are unavailable");
        };
        let label = session::run_label(&grant.client_name, &params, number);
        let session = Arc::new(Session::new(
            grant.id.clone(),
            grant.person.clone(),
            grant.scope.clone(),
            label,
            params,
            Arc::clone(&self.directory),
            self.http.clone(),
        ));
        let answer = match session.initialize(request).await {
            Ok(answer) => answer,
            Err(message) => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                return json_reply(StatusCode::OK, &session::error(&id, -32000, &message));
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        lock(&self.sessions).insert(id.clone(), session);
        session_reply(
            &session::Reply {
                messages: vec![answer],
                status: 200,
            },
            &hyper::HeaderMap::new(),
            Some(&id),
        )
    }
}

/// What a path names on the door's MCP endpoint: `Some(None)` for the
/// door itself (`/` or `/mcp`), `Some(Some(workspace))` for
/// `/w/<workspace>` (or `/w/<workspace>/mcp`), `None` for anything else.
#[expect(
    clippy::option_option,
    reason = "the same answer as `oauth::resource_scope`: not found, the door itself, or one workspace"
)]
fn resource_of(path: &str) -> Option<Option<String>> {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/mcp" {
        return Some(None);
    }
    pinned_workspace(path).map(Some)
}

/// The workspace `/w/<workspace>` (or `/w/<workspace>/mcp`) names: a
/// canonical UUID, so a path can never smuggle anything else.
pub(crate) fn pinned_workspace(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/w/")?;
    let workspace = rest.strip_suffix("/mcp").unwrap_or(rest);
    let parsed = uuid::Uuid::parse_str(workspace).ok()?;
    (!parsed.is_nil() && parsed.to_string() == workspace).then(|| workspace.to_owned())
}

fn session_reply(
    reply: &session::Reply,
    headers: &hyper::HeaderMap,
    created: Option<&str>,
) -> Reply {
    let mut response = match reply.messages.as_slice() {
        [] => text(
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::ACCEPTED),
            "",
        ),
        [only] => json_reply(StatusCode::OK, only),
        [.., last] if !accepts_events(headers) => json_reply(StatusCode::OK, last),
        messages => {
            let body = messages.iter().fold(String::new(), |mut body, message| {
                let _ = write!(body, "event: message\ndata: {message}\n\n");
                body
            });
            let mut response = Response::new(Full::new(Bytes::from(body)));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            response
        }
    };
    if let Some(id) = created
        && let Ok(value) = HeaderValue::from_str(id)
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn accepts_events(headers: &hyper::HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.contains("text/event-stream"))
}

fn bearer(headers: &hyper::HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn session_id(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ms()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_reply(status: StatusCode, value: &Value) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn text(status: StatusCode, message: &str) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(message.to_owned())));
    *response.status_mut() = status;
    response
}

fn html(status: StatusCode, page: &str) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(page.to_owned())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self' https: http://localhost:* http://127.0.0.1:*; frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The answer when the system's random source fails, so no secret, cookie
/// or challenge can be minted.
fn no_randomness() -> Reply {
    text(
        StatusCode::SERVICE_UNAVAILABLE,
        "no randomness is available; try again shortly",
    )
}

/// Send the browser back to the client at `redirect_uri` with `params`.
fn redirect_back(redirect_uri: &str, params: &[(&str, &str)]) -> Reply {
    match oauth::redirect_to(redirect_uri, params) {
        Some(location) => redirect(&location),
        None => html(
            StatusCode::BAD_REQUEST,
            &pages::message(
                "This app cannot connect",
                "Its return address is not a URL.",
            ),
        ),
    }
}

fn redirect(location: &str) -> Reply {
    let mut response = text(StatusCode::SEE_OTHER, "");
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Serve `door` on `listener` until the process ends.
pub async fn serve(door: Arc<Door>, listener: tokio::net::TcpListener) {
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let door = Arc::clone(&door);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let door = Arc::clone(&door);
                async move { Ok::<_, std::convert::Infallible>(door.respond(request).await) }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::debug!(%error, "door connection ended");
            }
        });
    }
}

#[cfg(test)]
mod tests;

// The hosts the tests open keep workspaces in automerge documents, which run
// in a worker process this test binary serves.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
