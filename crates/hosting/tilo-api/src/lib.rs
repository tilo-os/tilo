//! The Tilo API server: one MCP endpoint an agent
//! connects to with nothing configured but its URL, as in
//!
//! ```text
//! claude mcp add --transport http tilo https://mcp.tilo.sh
//! ```
//!
//! The agent finds out how to authorize from the server itself (the MCP
//! authorization profile: OAuth 2.1, [`oauth`]). The server sends the person
//! to their own Tilo, which asks them once, naming the agent, what it may
//! reach and until when, and then signs them in with a device of theirs
//! ([`sign_in`]) and certifies a key the server made for this one grant
//! ([`grant_key`]):
//!
//! - **All my workspaces**: the agent lists and creates workspaces, and
//!   names the workspace each time it acts;
//! - **One workspace**: the agent is pinned to it and cannot see or reach
//!   another. Pointing the agent at `https://mcp.tilo.sh/w/<workspace>` asks for exactly
//!   that.
//!
//! The server holds no standing authority over anyone: it acts only with a
//! grant's key, which the relay the person's workspaces live on admits as
//! its person's agent, within the grant's scope and until it expires
//! ([`relay`]). It serves each MCP session itself, with the agent surface
//! `tilo mcp` serves, over the grant's own session with the relay, where
//! record actions run on the relay's app workers ([`session`]). Every
//! session is one agent run, attributed separately in History and named for
//! the person ("Claude Code #1"). The person revokes a grant at `/grants`;
//! its tokens and sessions end at once and its key is forgotten.

pub mod account;
pub mod content;
pub mod direct;
pub mod grant_key;
pub mod keys;
pub mod oauth;
pub mod pages;
pub mod relay;
pub mod session;
pub mod sign_in;
pub mod store;
pub mod v1;

use grant_key::GrantKey;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{self, HeaderValue},
};
use relay::{Connection, Relay};
use serde_json::{Value, json};
use session::{SESSION_HEADER, Session};
use std::{
    collections::HashMap,
    fmt::Write as _,
    io,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use store::Store;
use tilo_user_identity::AgentScope;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 4_096;
const MAX_SESSIONS_PER_GRANT: usize = 64;
const SESSION_IDLE: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_PENDING: usize = 4_096;
/// The prefix of a key's API run in the session table.
const API_SESSION: &str = "api ";
/// How long a person has to sign in and decide.
const PENDING_LIFETIME_MS: u64 = 15 * 60 * 1000;

/// What the server is called and where people sign in.
#[derive(Clone, Debug)]
pub struct Config {
    /// The server's own origin, as agents reach it (`https://mcp.tilo.sh`): the
    /// OAuth issuer and the protected resource. No trailing slash.
    pub issuer: String,
    /// The person's Tilo on the web, which signs them in to the server.
    pub web_entry: String,
    /// The public origin of the relay the person's workspaces live on
    /// (`https://eu1.tilo.sh`).
    pub relay: String,
}

/// What a sign-in in progress is for.
enum Purpose {
    /// An OAuth client's authorization request.
    Agent(oauth::AuthorizeRequest),
    /// An API key called `name`. With a poll secret's digest, the key goes
    /// to whoever polls with that secret ([`keys`]); without one, the page
    /// the person returns to shows it once.
    Key { name: String, poll: Option<String> },
}

/// The grant the person is asked to certify.
#[derive(Clone, Copy, Debug)]
struct GrantAsk {
    /// The workspace the agent was pointed at, or all of them. The
    /// certificate they answer with may be narrower — one of their
    /// workspaces where all were asked for.
    scope: AgentScope,
    /// A server key: an API key reaching all the person's workspaces,
    /// which lasts until revoked unless they chose an expiry.
    server_key: bool,
    /// When the grant expires, in Unix milliseconds; `None` for a server
    /// key that lasts until revoked.
    expires_ms: Option<u64>,
    /// The key may operate web pages in Browser: navigate, fill, select,
    /// click, press, scroll, point and drag in its workspace's Browser
    /// without asking the person each time. Submitting a page still asks.
    browser_pages: bool,
}

impl GrantAsk {
    /// An agent's grant reaching `scope`, for as long as a grant may last.
    fn bounded(scope: AgentScope, now: u64) -> Self {
        Self {
            scope,
            server_key: false,
            expires_ms: Some(
                now.saturating_add(
                    u64::try_from(grant_key::GRANT_LIFETIME.as_millis()).unwrap_or(0),
                ),
            ),
            browser_pages: false,
        }
    }
}

/// An authorization in progress: the agent's request or an API key,
/// waiting for the person's Tilo to answer it.
struct Pending {
    purpose: Purpose,
    challenge: [u8; 32],
    /// The key made for the grant, waiting for the person's certificate.
    key: ed25519_dalek::SigningKey,
    /// What the person is asked to approve.
    grant: GrantAsk,
    expires_ms: u64,
    /// Where the person's Tilo answers it.
    location: String,
}

/// The API server.
pub struct Server {
    config: Config,
    /// `<web_entry>/sign-in`, where the person's Tilo signs them in.
    sign_in_entry: url::Url,
    store: Store,
    relay: Arc<Relay>,
    browsers: sign_in::Browsers,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Each grant's session with the relay, by grant id, which all its runs,
    /// uploads and account calls share: the relay keeps one session per key,
    /// so a second would cut the first off. It ends with the last run that
    /// holds it, or when the grant is revoked.
    connections: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Weak<Connection>>>>>,
    /// Per signed-in browser, the token its grants page forms carry.
    page_tokens: Mutex<HashMap<String, String>>,
    /// The keys approved for a poller, by the poll secret's digest.
    approved_keys: Mutex<HashMap<String, keys::Approval>>,
    /// Reaches the site (to see that its sign-in page is served) and the
    /// URLs webhooks post to.
    http: tilo_net::Client,
    /// What the API keeps besides the store: remembered answers, webhook
    /// deliveries.
    v1: v1::State,
    /// Whether the site serves the sign-in page every approval lands on, as
    /// last seen and when ([`Self::sign_in_page`]).
    sign_in_seen: Mutex<Option<(std::time::Instant, Result<(), String>)>>,
}

/// How long what the site answered for its sign-in page is trusted.
const SIGN_IN_SEEN_FOR: Duration = Duration::from_secs(60);

/// What every answer's body is: whole, or a stream (server-sent events).
pub type Body = http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>;

/// One answer of the server.
pub type Reply = Response<Body>;

/// `bytes` as a whole body.
pub fn body(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).boxed()
}

impl Server {
    /// # Errors
    /// The web entry is not a URL, or the HTTP client for the relay and the
    /// site could not be built.
    pub fn new(config: &Config, store: Store) -> io::Result<Arc<Self>> {
        let http = tilo_net::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirects(tilo_net::Redirects::None)
            .build()
            .map_err(io::Error::other)?;
        let web_entry = config.web_entry.trim_end_matches('/').to_owned();
        let sign_in_entry = url::Url::parse(&format!("{web_entry}/sign-in"))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Ok(Arc::new(Self {
            relay: Arc::new(Relay::new(&http, &config.relay)),
            http,
            config: Config {
                issuer: config.issuer.trim_end_matches('/').to_owned(),
                relay: config.relay.trim_end_matches('/').to_owned(),
                web_entry,
            },
            sign_in_entry,
            store,
            browsers: sign_in::Browsers::default(),
            pending: Mutex::default(),
            sessions: Mutex::default(),
            connections: Mutex::default(),
            page_tokens: Mutex::default(),
            approved_keys: Mutex::default(),
            sign_in_seen: Mutex::default(),
            v1: v1::State::default(),
        }))
    }

    /// Whether the site serves the sign-in page every key and agent approval
    /// lands on (`<web entry>/sign-in`): `Err` with what it answered when it
    /// does not, so a site deployed without it shows on `/health` and in the
    /// log instead of as a dead link in front of the person approving.
    pub async fn sign_in_page(&self) -> Result<(), String> {
        if let Some((at, seen)) = lock(&self.sign_in_seen).as_ref()
            && at.elapsed() < SIGN_IN_SEEN_FOR
        {
            return seen.clone();
        }
        let url = self.sign_in_entry.as_str();
        let seen = match self
            .http
            .get(url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(answer) if answer.status().is_success() => Ok(()),
            Ok(answer) => Err(format!("{url} answers {}", answer.status())),
            Err(error) => Err(format!("{url} was not reached: {error}")),
        };
        if let Err(why) = &seen {
            tracing::error!(%why, "the site serves no sign-in page: no key or agent can be approved");
        }
        *lock(&self.sign_in_seen) = Some((std::time::Instant::now(), seen.clone()));
        seen
    }

    /// The live MCP sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// Answer one HTTP request.
    pub async fn respond(self: &Arc<Self>, request: Request<Incoming>) -> Reply {
        let (parts, body) = request.into_parts();
        let uploading = v1::uploads(&parts.method, parts.uri.path());
        let limit = if uploading {
            content::MAX_CONTENT_BYTES
        } else {
            MAX_BODY_BYTES
        };
        let body = match Limited::new(body, limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) if uploading => return content::too_large(),
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
            // The server itself answers whatever the site does; what the site's
            // sign-in page answers is reported beside it.
            (&Method::GET, "/health") => {
                let sign_in = match self.sign_in_page().await {
                    Ok(()) => "served".to_owned(),
                    Err(why) => why,
                };
                json_reply(StatusCode::OK, &json!({"ok": true, "sign_in": sign_in}))
            }
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
            (&Method::GET, oauth::AUTHORIZE_PATH) => self.authorize(query),
            (&Method::POST, "/sign-in") => self.signed_in(body),
            (&Method::POST, oauth::TOKEN_PATH) => self.token(body),
            (&Method::POST, oauth::REVOKE_PATH) => self.revoke(body).await,
            // Boxed: the API's calls hold much across their waits, which the
            // other routes' futures need not carry.
            (method, path) if v1::serves(path) => {
                Box::pin(v1::respond(
                    self,
                    &v1::Request {
                        method,
                        path,
                        query,
                        headers,
                        body,
                    },
                ))
                .await
            }
            (&Method::POST, keys::NEW_PATH) => self.new_key(body),
            (&Method::POST, keys::POLL_PATH) => self.poll_key(body),
            (&Method::GET, path) if let Some(id) = path.strip_prefix(keys::APPROVE_PREFIX) => {
                self.approve_key(id)
            }
            (&Method::POST, "/grants/keys") => self.new_key_from_page(headers, body),
            (&Method::GET, "/grants") => self.grants_page(headers),
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

    /// The resource URL agents use: the server, or one workspace of it.
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

    fn authorize(&self, query: &str) -> Reply {
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
        let scope = match request.pinned.as_deref() {
            Some(workspace) => match AgentScope::parse(workspace) {
                Some(scope) => scope,
                None => {
                    return redirect_back(&request.redirect_uri, &[("error", "invalid_target")]);
                }
            },
            None => AgentScope::AllWorkspaces,
        };
        let now = now_ms();
        let mut challenge = [0u8; 32];
        let (Ok(()), Ok(id), Ok(key)) = (
            getrandom::fill(&mut challenge),
            store::secret(24),
            GrantKey::generate(),
        ) else {
            return no_randomness();
        };
        let client_name = client.map(|client| client.name).unwrap_or_default();
        match self.begin(
            Purpose::Agent(request),
            &client_name,
            GrantAsk::bounded(scope, now),
            (id, challenge, key),
            now,
        ) {
            Ok(location) => redirect(&location),
            Err(why) => text(StatusCode::SERVICE_UNAVAILABLE, why),
        }
    }

    /// Ask the person's Tilo to approve `purpose` as the grant `grant`, shown
    /// as `client`: the sign-in in progress `id`, with its `challenge` and the
    /// grant's `key`. Answers where the person's Tilo answers it.
    fn begin(
        &self,
        purpose: Purpose,
        client: &str,
        grant: GrantAsk,
        (id, challenge, key): (String, [u8; 32], ed25519_dalek::SigningKey),
        now: u64,
    ) -> Result<String, &'static str> {
        let location = self.sign_in_url(&SignInAsk {
            pending: &id,
            challenge: &challenge,
            client,
            key: &key,
            grant,
        });
        let mut pending = lock(&self.pending);
        pending.retain(|_, pending| pending.expires_ms > now);
        if pending.len() >= MAX_PENDING {
            return Err("too many sign-ins in progress");
        }
        pending.insert(
            id,
            Pending {
                purpose,
                challenge,
                key,
                grant,
                expires_ms: now.saturating_add(PENDING_LIFETIME_MS),
                location: location.clone(),
            },
        );
        drop(pending);
        Ok(location)
    }

    /// Where the person's Tilo answers the authorization `ask`.
    fn sign_in_url(&self, ask: &SignInAsk<'_>) -> String {
        let mut url = self.sign_in_entry.clone();
        url.query_pairs_mut()
            .append_pair("audience", &self.config.issuer)
            .append_pair("challenge", &hex::encode(ask.challenge))
            .append_pair("return", &format!("{}/sign-in", self.config.issuer))
            .append_pair("pending", ask.pending)
            .append_pair("client", ask.client)
            .append_pair(
                "grant_key",
                &hex::encode(ask.key.verifying_key().to_bytes()),
            )
            .append_pair("grant_scope", &ask.grant.scope.name())
            .append_pair(
                "grant_expires",
                &ask.grant
                    .expires_ms
                    .map_or_else(|| "never".to_owned(), |ms| ms.to_string()),
            );
        if ask.grant.server_key {
            url.query_pairs_mut()
                .append_pair("grant_kind", "server_key");
        }
        if ask.grant.browser_pages {
            url.query_pairs_mut().append_pair("grant_pages", "1");
        }
        url.into()
    }

    /// `POST /sign-in`: the person's Tilo answers a pending authorization,
    /// declined (`decision=deny`) or with a sign-in proof and the certificate
    /// it gave the grant's key.
    fn signed_in(&self, body: &[u8]) -> Reply {
        let form = oauth::form(body).unwrap_or_default();
        let now = now_ms();
        // Answered once, whatever the answer: its challenge and key are spent.
        let Some(pending) = form.get("pending").and_then(|id| {
            lock(&self.pending)
                .remove(id)
                .filter(|pending| pending.expires_ms > now)
        }) else {
            return html(
                StatusCode::BAD_REQUEST,
                &pages::message("Sign-in expired", "Start connecting the agent again."),
            );
        };
        let request = match &pending.purpose {
            Purpose::Agent(request) => request,
            Purpose::Key { name, poll } => {
                return self.key_signed_in(&pending, name, poll.as_deref(), &form, now);
            }
        };
        let back = |params: &[(&str, &str)]| {
            let mut params = params.to_vec();
            if let Some(state) = request.state.as_deref() {
                params.push(("state", state));
            }
            redirect_back(&request.redirect_uri, &params)
        };
        if form.get("decision").map(String::as_str) == Some("deny") {
            return back(&[("error", "access_denied")]);
        }
        let (Some(proof), Some(cert)) = (
            form.get("proof")
                .and_then(|proof| sign_in::decode_proof(proof)),
            form.get("grant")
                .and_then(|cert| sign_in::decode_cert(cert)),
        ) else {
            return html(
                StatusCode::BAD_REQUEST,
                &pages::message("Sign-in failed", "The sign-in was incomplete."),
            );
        };
        let key = match self.check_answer(&pending, &proof, cert, now) {
            Ok(key) => key,
            Err(reply) => return *reply,
        };
        let Some(client) = self.store.client(&request.client_id) else {
            return back(&[("error", "unauthorized_client")]);
        };
        let Ok(code) = self.store.consent(
            &key,
            &client,
            request.redirect_uri.clone(),
            request.challenge.clone(),
            now,
        ) else {
            return back(&[("error", "temporarily_unavailable")]);
        };
        let mut response = back(&[("code", &code)]);
        // The browser is signed in to the server too, for its grants page.
        if let Ok(cookie) = self.browsers.sign_in(key.person(), now)
            && let Ok(value) = HeaderValue::from_str(&sign_in::set_cookie(&cookie))
        {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        response
    }

    /// Check the person's Tilo's answer to `pending`: the sign-in proof, and
    /// the certificate it gave the grant's key. The grant's key, or the page
    /// that says why not.
    fn check_answer(
        &self,
        pending: &Pending,
        proof: &tilo_user_identity::SignInProof,
        cert: tilo_user_identity::DeviceCert,
        now: u64,
    ) -> Result<GrantKey, Box<Reply>> {
        let now_ns = now.saturating_mul(1_000_000);
        let refused = |why: &str| {
            tracing::info!(reason = why, "sign-in refused");
            Box::new(html(
                StatusCode::FORBIDDEN,
                &pages::message("Sign-in refused", why),
            ))
        };
        // The proof: whose it is, for this server and this challenge. The
        // grant's own certificate, which the person's root signed, is checked
        // against their revocations by the relay at every handshake.
        if let Err(error) = sign_in::verify(
            proof,
            &self.config.issuer,
            &pending.challenge,
            &tilo_user_identity::RevocationList::new(),
            now_ns,
        ) {
            return Err(refused(&error.to_string()));
        }
        if cert.user_public_key != proof.cert.user_public_key {
            return Err(refused(
                "the grant was certified by someone else than who signed in",
            ));
        }
        let outlasts = match (pending.grant.expires_ms, cert.expires_at_ns) {
            (Some(asked), Some(expiry)) => expiry > asked.saturating_mul(1_000_000),
            (Some(_), None) => true,
            (None, _) => false,
        };
        if outlasts {
            return Err(refused("the grant was certified for longer than was asked"));
        }
        GrantKey::new(
            pending.key.clone(),
            cert,
            pending.grant.scope,
            pending.grant.server_key,
            pending.grant.browser_pages,
            now_ns,
        )
        .map_err(|error| refused(&error.to_string()))
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
            && let Ok(revoked) = self.store.revoke_token(token, client, now_ms())
        {
            for grant in revoked {
                self.end_sessions_of(&grant).await;
            }
        }
        text(StatusCode::OK, "")
    }

    fn grants_page(&self, headers: &hyper::HeaderMap) -> Reply {
        let Some(person) = self.browsers.person(headers, now_ms()) else {
            return html(
                StatusCode::UNAUTHORIZED,
                &pages::message(
                    "Sign in first",
                    "Connect an agent to sign in, then manage it here.",
                ),
            );
        };
        let grants = self.store.grants_of(&person);
        let Ok(csrf) = self.page_token(headers) else {
            return no_randomness();
        };
        html(StatusCode::OK, &pages::grants(&grants, &csrf))
    }

    /// Whether `form` was posted from the grants page of the browser that
    /// sent `headers`.
    fn posted_from_grants_page(
        &self,
        headers: &hyper::HeaderMap,
        form: &HashMap<String, String>,
    ) -> bool {
        self.page_token(headers).is_ok_and(|expected| {
            form.get("csrf")
                .is_some_and(|csrf| constant_time_eq(csrf.as_bytes(), expected.as_bytes()))
        })
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
        if !self.posted_from_grants_page(headers, &form) {
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

    /// Revoke `person`'s grant `grant` and every grant it minted: their
    /// tokens stop working and their sessions end now. Answers whether it
    /// was theirs and live.
    ///
    /// # Errors
    /// The store could not be written.
    pub async fn revoke_grant(&self, person: &str, grant: &str) -> io::Result<bool> {
        let revoked = self.store.revoke_grant(person, grant, now_ms())?;
        for each in &revoked {
            self.end_sessions_of(each).await;
        }
        Ok(!revoked.is_empty())
    }

    async fn end_sessions_of(&self, grant: &str) {
        let ended: Vec<Arc<Session>> = lock(&self.sessions)
            .extract_if(|_, session| session.grant == grant)
            .map(|(_, session)| session)
            .collect();
        for session in ended {
            session.close().await;
        }
        let shared = lock(&self.connections).remove(grant);
        if let Some(shared) = shared
            && let Some(connection) = shared.lock().await.upgrade()
        {
            connection.device.close();
        }
    }

    /// The MCP endpoint, at the server's root or pinned to `/w/<workspace>`.
    async fn mcp(
        self: &Arc<Self>,
        method: &Method,
        pinned: Option<&str>,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let grant = match bearer(headers).map(|token| self.store.access(token, now_ms())) {
            Some(Ok(grant)) => grant,
            Some(Err(denied)) => return self.challenge(pinned, Some(denied)),
            None => return self.challenge(pinned, None),
        };
        if !oauth::scope_reaches(&grant.scope, pinned) {
            return self.insufficient(pinned);
        }
        match *method {
            Method::POST => self.post(grant, pinned, headers, body).await,
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

    /// 401 naming where the agent learns how to get a token (RFC 9728),
    /// and, for a token it was sent, why that token reaches nothing
    /// ([`denial`]).
    fn challenge(&self, pinned: Option<&str>, denied: Option<store::Denied>) -> Reply {
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
        let mut reply = match denied {
            Some(denied) => {
                let refusal = denial(denied);
                let _ = write!(
                    value,
                    ", error=\"invalid_token\", error_description=\"{}: {}\"",
                    denied.code(),
                    refusal
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                );
                json_reply(StatusCode::UNAUTHORIZED, &refusal)
            }
            None => text(StatusCode::UNAUTHORIZED, "sign in to use Tilo"),
        };
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
        pinned: Option<&str>,
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
            (None, true) => self.open(grant, pinned, &request).await,
            (Some(id), _) => {
                let session = lock(&self.sessions)
                    .get(&id)
                    .filter(|session| session.grant == grant.id)
                    .cloned();
                let Some(session) = session else {
                    return json_reply(
                        StatusCode::NOT_FOUND,
                        &json!({"jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null),
                                "error": {"code": -32600,
                                          "message": "unknown session; initialize again"}}),
                    );
                };
                let reply = session.handle(request).await;
                session_reply(&reply, headers, None)
            }
            // A JSON-RPC client reads the refusal as it reads any answer: a
            // plain-text body reads to it as no answer at all.
            (None, false) => json_reply(
                StatusCode::BAD_REQUEST,
                &json!({"jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null),
                        "error": {"code": -32600,
                                  "message": "no Mcp-Session-Id: this request belongs to a session, which `initialize` opens (an `initialize` refused opens none)"}}),
            ),
        }
    }

    /// `initialize`: a new agent run under `grant`, in the workspace the URL
    /// is pinned to, if it is.
    async fn open(
        self: &Arc<Self>,
        grant: store::Grant,
        pinned: Option<&str>,
        request: &Value,
    ) -> Reply {
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
        let scope = pinned.map_or_else(
            || grant.scope.clone(),
            |workspace| store::Scope::Workspace(workspace.to_owned()),
        );
        let session = match self.new_session(&grant, scope, label).await {
            Ok(session) => Arc::new(session),
            Err(message) => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                return json_reply(StatusCode::OK, &session::error(&id, -32000, &message));
            }
        };
        let answer = match session.initialize(request).await {
            Ok(answer) => answer,
            Err(refused) => return json_reply(StatusCode::OK, &refused),
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

impl Server {
    /// The grant whose key `headers` carry, or the refusal saying why there
    /// is none.
    pub(crate) fn grant_of(&self, headers: &hyper::HeaderMap) -> Result<store::Grant, Box<Reply>> {
        match bearer(headers).map(|token| self.store.access(token, now_ms())) {
            Some(Ok(grant)) => Ok(grant),
            Some(Err(denied)) => Err(Box::new(json_reply(
                StatusCode::UNAUTHORIZED,
                &denial(denied),
            ))),
            None => Err(Box::new(json_reply(
                StatusCode::UNAUTHORIZED,
                &json!({"error": {
                    "code": "unauthorized",
                    "message": "send a key: Authorization: Bearer <key>",
                    "next_steps": ["create one with `tilo keys create`"],
                }}),
            ))),
        }
    }

    /// `grant`'s run for the calls of the API: one per key, named for it
    /// ("Visa backend #3"), opened the first time and kept with the key's
    /// other sessions, so revoking the key ends it.
    pub(crate) async fn api_session(
        self: &Arc<Self>,
        grant: &store::Grant,
    ) -> Result<Arc<Session>, v1::errors::ApiError> {
        let id = format!("{API_SESSION}{}", grant.id);
        let open = lock(&self.sessions)
            .get(&id)
            .filter(|session| session.connection().is_live())
            .cloned();
        if let Some(open) = open {
            return Ok(open);
        }
        let unavailable =
            |why: String| v1::errors::ApiError::new("unavailable", why).next("try again shortly");
        let number = self
            .store
            .next_run(&grant.person)
            .map_err(|_| unavailable("this key cannot act now".to_owned()))?;
        let label = format!("{} #{number}", grant.client_name);
        let opened = Arc::new(
            self.new_session(grant, grant.scope.clone(), label)
                .await
                .map_err(unavailable)?,
        );
        let initialize = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tilo-api", "version": env!("CARGO_PKG_VERSION")},
        }});
        if let Err(refused) = opened.initialize(&initialize).await {
            let why = refused
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the workspace refused");
            return Err(unavailable(why.to_owned()));
        }
        lock(&self.sessions).insert(id, Arc::clone(&opened));
        Ok(opened)
    }

    /// A new run of `grant` acting in `scope`, named `label`: the grant's own
    /// session with the relay, and for a minted grant the session of the
    /// grant that minted it, which asks for its invitation.
    pub(crate) async fn new_session(
        self: &Arc<Self>,
        grant: &store::Grant,
        scope: store::Scope,
        label: String,
    ) -> Result<Session, String> {
        let key = grant.key().ok_or("this key cannot act now")?;
        let connection = self
            .connection(&grant.id, &key)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Session::new(
            grant.id.clone(),
            scope,
            label,
            Arc::clone(&self.relay),
            connection,
            Arc::downgrade(self),
        ))
    }
}

impl Server {
    /// `grant`'s session with the relay: the live one its other runs hold,
    /// else a new one. Runs starting at once wait for the one connecting.
    async fn connection(
        &self,
        grant: &str,
        key: &GrantKey,
    ) -> Result<Arc<Connection>, relay::RelayError> {
        let slot = {
            let mut connections = lock(&self.connections);
            // Forget the grants whose session ended with their last run.
            connections.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot.try_lock().map_or(true, |held| held.strong_count() > 0)
            });
            Arc::clone(connections.entry(grant.to_owned()).or_default())
        };
        let mut shared = slot.lock().await;
        if let Some(live) = shared.upgrade().filter(|live| live.is_live()) {
            return Ok(live);
        }
        let connection = Arc::new(self.relay.connect(key).await?);
        *shared = Arc::downgrade(&connection);
        drop(shared);
        Ok(connection)
    }
}

/// What the server asks the person's Tilo to answer.
struct SignInAsk<'a> {
    pending: &'a str,
    challenge: &'a [u8; 32],
    client: &'a str,
    key: &'a ed25519_dalek::SigningKey,
    grant: GrantAsk,
}

/// What a path names on the server's MCP endpoint: `Some(None)` for the
/// server itself (`/` or `/mcp`), `Some(Some(workspace))` for
/// `/w/<workspace>` (or `/w/<workspace>/mcp`), `None` for anything else.
#[expect(
    clippy::option_option,
    reason = "the same answer as `oauth::resource_scope`: not found, the server itself, or one workspace"
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
            let body_text = messages.iter().fold(String::new(), |mut text, message| {
                let _ = write!(text, "event: message\ndata: {message}\n\n");
                text
            });
            let mut response = Response::new(body(body_text));
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

/// Why a token the server was sent reaches nothing, as every refusal of the server is
/// typed: `{"error": {code, message, next_steps}}`, with when it was revoked
/// (`revoked_at_ms`) or expired (`expired_at_ms`) when the server knows. A
/// caller tells a revoked key from an expired one and from one that never
/// was a key by the code alone.
pub(crate) fn denial(denied: store::Denied) -> Value {
    let (message, next, when) = match denied {
        store::Denied::Unknown => (
            "this is not a key or token this server issued".to_owned(),
            "check the key you pass, or create one with `tilo keys create`",
            None,
        ),
        store::Denied::Revoked { at_ms } => (
            at_ms.map_or_else(
                || "this key was revoked".to_owned(),
                |at| format!("this key was revoked on {}", pages::date(at)),
            ),
            "ask the person for a new key",
            at_ms.map(|at| ("revoked_at_ms", at)),
        ),
        store::Denied::Expired { at_ms } => (
            at_ms.map_or_else(
                || "this key expired".to_owned(),
                |at| format!("this key expired on {}", pages::date(at)),
            ),
            "ask the person for a new key, or mint one again",
            at_ms.map(|at| ("expired_at_ms", at)),
        ),
    };
    let mut error = json!({"code": denied.code(), "message": message, "next_steps": [next]});
    if let (Some((field, at)), Some(fields)) = (when, error.as_object_mut()) {
        fields.insert(field.into(), json!(at));
    }
    json!({ "error": error })
}

fn json_reply(status: StatusCode, value: &Value) -> Reply {
    let mut response = Response::new(body(value.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn text(status: StatusCode, message: &str) -> Reply {
    let mut response = Response::new(body(message.to_owned()));
    *response.status_mut() = status;
    response
}

fn html(status: StatusCode, page: &str) -> Reply {
    let mut response = Response::new(body(page.to_owned()));
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

/// Serve `server` on `listener` until the process ends.
pub async fn serve(server: Arc<Server>, listener: tokio::net::TcpListener) {
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    v1::webhooks::Deliveries::start_all(&server);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, std::convert::Infallible>(server.respond(request).await) }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::debug!(%error, "API connection ended");
            }
        });
    }
}

#[cfg(test)]
mod tests;
