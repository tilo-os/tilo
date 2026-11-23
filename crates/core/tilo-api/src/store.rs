//! What the server remembers: the clients that registered, the grants people
//! gave them and the tokens issued under each grant.
//!
//! Tokens are bearer secrets, so only their SHA-256 digests are kept: a copy
//! of the file names no credential a caller could present. Authorization
//! codes live in memory only; they are redeemed within a minute, and a
//! restart simply makes the client ask again.
//!
//! A grant is the person's decision, and revoking it is final: its tokens stop
//! working at once, its refresh token cannot mint new ones, the sessions
//! opened under it are closed by the server ([`crate::Server::revoke_grant`]),
//! and its key ([`GrantKey`]) is forgotten, so nothing can act with it
//! again. The file holds each live grant's key, so it is private to the server.

use crate::grant_key::GrantKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// How long an access token works before the client refreshes it.
pub const ACCESS_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// How long a refresh token waits unused before the client must ask again.
pub const REFRESH_LIFETIME: Duration = Duration::from_secs(60 * 60 * 24 * 30);
/// How long an authorization code may wait before it is redeemed.
pub const CODE_LIFETIME: Duration = Duration::from_secs(60);
/// The most clients that may be registered at once. Registration is open, as
/// the MCP authorization profile needs it to be, so it is bounded; the
/// oldest client nobody holds a grant for gives way first.
const MAX_CLIENTS: usize = 10_000;
const MAX_CODES: usize = 4_096;
/// The most keys of one person this store keeps that it did not issue
/// itself ([`Store::keep_carried`]).
pub const MAX_CARRIED_PER_PERSON: usize = 32;
/// How often using a token writes when its grant was last used to the file:
/// a use is noted in memory at once, and kept at most this much later.
const LAST_USED_SAVE_MS: u64 = 60 * 1000;

/// What a grant lets its agent reach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every workspace the person has, and making new ones. Every action names
    /// the workspace it is for.
    Account,
    /// One workspace, and nothing else: the agent cannot see or name others.
    Workspace(String),
}

impl Scope {
    /// The scope as keys name it: `all`, or the workspace's id.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Account => "all".to_owned(),
            Self::Workspace(id) => id.clone(),
        }
    }
}

/// A client that registered itself (RFC 7591). Only public clients are
/// registered: an agent on a person's machine keeps no secret, so PKCE is
/// what binds a code to the client that asked for it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub issued_at_ms: u64,
}

/// How a grant came to be.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// An agent the person approved through OAuth: it holds the client's
    /// access and refresh tokens.
    #[default]
    Agent,
    /// An API key the person approved: one bearer token, lasting as long
    /// as the grant's certificate — until revoked, for a key reaching all
    /// their workspaces, unless they chose an expiry.
    Key,
    /// A grant for one workspace that the all-workspaces grant `parent`
    /// minted, with a bearer token like a key's. It ends with its parent.
    Minted { parent: String },
}

/// A person's consent: this client, acting for this person, with this scope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    /// The person's identity root key, hex.
    pub person: String,
    /// The OAuth client holding the grant; empty for a key or a minted grant.
    pub client_id: String,
    /// What the person saw the client called when they agreed, or the name
    /// a key or a minted grant was given. Its runs are named after it.
    pub client_name: String,
    pub scope: Scope,
    #[serde(default)]
    pub kind: GrantKind,
    /// When the grant's certificate expires, in Unix milliseconds; `None`
    /// for a server key that lasts until revoked.
    #[serde(default)]
    pub expires_ms: Option<u64>,
    /// The key may operate web pages in Browser without asking its person
    /// each time: what its certificate carries, and what a key made before
    /// the person could choose does not.
    #[serde(default)]
    pub browser_pages: bool,
    pub created_ms: u64,
    /// When a token of the grant last reached the server, in Unix
    /// milliseconds.
    #[serde(default)]
    pub last_used_ms: Option<u64>,
    /// The device of the person that approved the grant, by the name its
    /// certificate gives it.
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub revoked_ms: Option<u64>,
    /// The grant's key and the person's certificate for it, until the grant
    /// is revoked.
    #[serde(default)]
    key: Option<KeptKey>,
}

/// A grant's key as the file keeps it.
#[derive(Clone, Serialize, Deserialize)]
struct KeptKey {
    /// The key's secret bytes, hex.
    secret: String,
    /// The certificate's canonical encoding, hex.
    cert: String,
}

impl std::fmt::Debug for KeptKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeptKey([redacted])")
    }
}

impl Grant {
    #[must_use]
    pub fn live(&self) -> bool {
        self.revoked_ms.is_none() && self.key.is_some()
    }

    /// The key the server acts with for this grant, while it is live.
    #[must_use]
    pub fn key(&self) -> Option<GrantKey> {
        let kept = self.key.as_ref().filter(|_| self.revoked_ms.is_none())?;
        GrantKey::from_kept(
            &hex::decode(&kept.secret).ok()?,
            &hex::decode(&kept.cert).ok()?,
        )
        .ok()
        .map(|key| key.named(&self.client_name))
    }

    /// Whether the grant is live and its certificate holds at `now_ms`: a
    /// grant past its certificate's expiry issues no token, and the agent
    /// asks the person again.
    #[must_use]
    pub fn usable(&self, now_ms: u64) -> bool {
        self.key()
            .is_some_and(|key| key.holds(now_ms.saturating_mul(1_000_000)))
    }

    fn revoke(&mut self, now_ms: u64) {
        self.revoked_ms.get_or_insert(now_ms);
        self.key = None;
    }

    /// The all-workspaces grant that minted this one, if one did.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        match &self.kind {
            GrantKind::Minted { parent } => Some(parent),
            GrantKind::Agent | GrantKind::Key => None,
        }
    }
}

fn scope_of(key: &GrantKey) -> Scope {
    key.scope()
        .workspace_name()
        .map_or(Scope::Account, Scope::Workspace)
}

fn expiry_ms(key: &GrantKey) -> Option<u64> {
    key.cert().expires_at_ns.map(|expiry| expiry / 1_000_000)
}

/// Whether `grant` is a live key this store keeps without having issued it:
/// one that was read from its token ([`Store::keep_carried`]), which no
/// token kept here names.
fn is_carried(state: &State, grant: &Grant) -> bool {
    grant.kind == GrantKind::Key
        && grant.live()
        && !state.tokens.values().any(|token| token.grant == grant.id)
}

/// Whether something kept here reads as the grant `id`: a grant it minted,
/// or a webhook it made.
fn is_read_as(state: &State, id: &str) -> bool {
    state
        .grants
        .values()
        .any(|grant| grant.parent() == Some(id))
        || state.webhooks.values().any(|hook| hook.grant == id)
}

/// Revoke `id` and every grant it minted, forgetting their tokens. Answers
/// the grants revoked.
fn revoke_with_minted(state: &mut State, id: &str, now_ms: u64) -> Vec<String> {
    let mut revoked = vec![id.to_owned()];
    revoked.extend(
        state
            .grants
            .values()
            .filter(|grant| grant.parent() == Some(id) && grant.live())
            .map(|grant| grant.id.clone()),
    );
    for each in &revoked {
        if let Some(grant) = state.grants.get_mut(each) {
            grant.revoke(now_ms);
        }
    }
    // A key's token is kept, its grant revoked, so the server tells whoever
    // still sends it that it was revoked rather than that it never was a
    // key. The tokens an agent's grant rotates through go.
    state
        .tokens
        .retain(|_, token| token.kind == TokenKind::Key || !revoked.contains(&token.grant));
    // Its webhooks read as the key, so they go with it.
    state
        .webhooks
        .retain(|_, hook| !revoked.contains(&hook.grant));
    revoked
}

/// Why a token reaches nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// Not a token the server issued, or one it no longer knows.
    Unknown,
    /// Its grant, or the grant that minted it, was revoked, at `at_ms`
    /// (Unix milliseconds) when the server knows.
    Revoked { at_ms: Option<u64> },
    /// It, or its grant's certificate, expired at `at_ms`.
    Expired { at_ms: Option<u64> },
}

impl Denied {
    /// Why `grant` reaches nothing at `now_ms`, if it does not: revoked, or
    /// past its certificate's expiry.
    fn of(grant: &Grant, now_ms: u64) -> Option<Self> {
        if grant.revoked_ms.is_some() || grant.key.is_none() {
            return Some(Self::Revoked {
                at_ms: grant.revoked_ms,
            });
        }
        (!grant.usable(now_ms)).then_some(Self::Expired {
            at_ms: grant.expires_ms,
        })
    }

    /// The code the server answers it with: `invalid_token`, `key_revoked`
    /// or `key_expired`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "invalid_token",
            Self::Revoked { .. } => "key_revoked",
            Self::Expired { .. } => "key_expired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TokenKind {
    Access,
    Refresh,
    /// A key's or a minted grant's one bearer token.
    Key,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Token {
    kind: TokenKind,
    grant: String,
    expires_ms: u64,
}

/// An authorization code waiting to be redeemed.
#[derive(Clone, Debug)]
pub struct Code {
    /// The grant the person agreed to. It takes effect when the code is
    /// redeemed, so a consent the client never completes leaves no grant.
    pub grant: Grant,
    pub client_id: String,
    pub redirect_uri: String,
    /// `BASE64URL(SHA256(code_verifier))`, as the client sent it.
    pub challenge: String,
    pub expires_ms: u64,
}

/// The pair a code or a refresh token is exchanged for.
#[derive(Clone, Debug)]
pub struct Issued {
    pub access: String,
    pub refresh: String,
    pub grant: Grant,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    clients: HashMap<String, Client>,
    grants: HashMap<String, Grant>,
    /// By the token's SHA-256 digest, hex.
    tokens: HashMap<String, Token>,
    /// How many agent runs each person has started through the server, so the
    /// next one is numbered after them ("Claude Code #3").
    runs: HashMap<String, u64>,
    /// The webhooks keys made, by id.
    #[serde(default)]
    webhooks: HashMap<String, Webhook>,
}

/// Where a workspace's events are posted for a key ([`crate::v1::webhooks`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    /// The key that made it, which its events are read as: revoked, it stops.
    pub grant: String,
    pub workspace: String,
    pub url: String,
    /// The kinds posted; all when empty.
    pub events: Vec<String>,
    pub path: Option<String>,
    /// What deliveries are signed with.
    pub secret: String,
    /// Where its stream was last delivered up to.
    pub cursor: Option<String>,
}

/// The server's memory, kept in one private file when it has a path.
pub struct Store {
    path: Option<PathBuf>,
    state: Mutex<State>,
    codes: Mutex<HashMap<String, Code>>,
    /// When the file was last written, in Unix milliseconds.
    saved_ms: AtomicU64,
}

/// Why a token request was refused, as RFC 6749 names it.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    InvalidGrant,
    InvalidClient,
}

impl Store {
    /// A store that forgets everything when the process ends.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: Mutex::default(),
            codes: Mutex::default(),
            saved_ms: AtomicU64::new(0),
        }
    }

    /// The store kept at `path`, empty when the file does not exist yet.
    ///
    /// # Errors
    /// The file exists but cannot be read privately or parsed.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let state = match tilo_files::native::private::read(&path) {
            Ok(file) => serde_json::from_reader(io::BufReader::new(file))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: Some(path),
            state: Mutex::new(state),
            codes: Mutex::default(),
            saved_ms: AtomicU64::new(0),
        })
    }

    fn change<T>(&self, change: impl FnOnce(&mut State) -> T) -> io::Result<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let answer = change(&mut state);
        self.save(&state)?;
        // Held until the file is written: two changes writing their
        // snapshots unlocked could leave the older one on disk.
        drop(state);
        Ok(answer)
    }

    /// Write `state` to the file, the caller holding its lock.
    fn save(&self, state: &State) -> io::Result<()> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
            tilo_files::native::private::write_atomic(path, &bytes)?;
            self.saved_ms
                .store(tilo_clock::unix_now_ms(), Ordering::Relaxed);
        }
        Ok(())
    }

    fn read<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(
            &self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Keep `webhook`.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn add_webhook(&self, webhook: Webhook) -> io::Result<()> {
        self.change(|state| {
            state.webhooks.insert(webhook.id.clone(), webhook);
        })
    }

    /// Forget `grant`'s webhook `id`: whether it was there.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn remove_webhook(&self, grant: &str, id: &str) -> io::Result<bool> {
        self.change(|state| {
            let theirs = state
                .webhooks
                .get(id)
                .is_some_and(|hook| hook.grant == grant);
            theirs && state.webhooks.remove(id).is_some()
        })
    }

    /// The webhooks, each grant's in `workspace`, or every one.
    #[must_use]
    pub fn webhooks(&self, of: Option<(&str, &str)>) -> Vec<Webhook> {
        self.read(|state| {
            let mut hooks: Vec<Webhook> = state
                .webhooks
                .values()
                .filter(|hook| {
                    of.is_none_or(|(grant, workspace)| {
                        hook.grant == grant && hook.workspace == workspace
                    })
                })
                .cloned()
                .collect();
            hooks.sort_by(|a, b| a.id.cmp(&b.id));
            hooks
        })
    }

    /// The webhook `id` was delivered up to `cursor`.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn webhook_delivered(&self, id: &str, cursor: &str) -> io::Result<()> {
        self.change(|state| {
            if let Some(hook) = state.webhooks.get_mut(id) {
                hook.cursor = Some(cursor.to_owned());
            }
        })
    }

    /// Register a public client.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn register(
        &self,
        name: String,
        redirect_uris: Vec<String>,
        now_ms: u64,
    ) -> io::Result<Client> {
        let client = Client {
            id: format!("tilo-client-{}", secret(16)?),
            name,
            redirect_uris,
            issued_at_ms: now_ms,
        };
        self.change(|state| {
            if state.clients.len() >= MAX_CLIENTS {
                let held: std::collections::HashSet<&str> = state
                    .grants
                    .values()
                    .map(|grant| grant.client_id.as_str())
                    .collect();
                let oldest = state
                    .clients
                    .values()
                    .filter(|client| !held.contains(client.id.as_str()))
                    .min_by_key(|client| client.issued_at_ms)
                    .map(|client| client.id.clone());
                if let Some(oldest) = oldest {
                    state.clients.remove(&oldest);
                }
            }
            state.clients.insert(client.id.clone(), client.clone());
        })?;
        Ok(client)
    }

    #[must_use]
    pub fn client(&self, id: &str) -> Option<Client> {
        self.read(|state| state.clients.get(id).cloned())
    }

    /// Record the person's consent, with the key they certified for it, and
    /// the code the client redeems for it.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn consent(
        &self,
        key: &GrantKey,
        client: &Client,
        redirect_uri: String,
        challenge: String,
        now_ms: u64,
    ) -> io::Result<String> {
        let grant = Grant {
            id: format!("grant-{}", secret(12)?),
            person: key.person(),
            client_id: client.id.clone(),
            client_name: client.name.clone(),
            scope: scope_of(key),
            kind: GrantKind::Agent,
            expires_ms: expiry_ms(key),
            browser_pages: key.browser_pages(),
            created_ms: now_ms,
            last_used_ms: None,
            created_by: None,
            revoked_ms: None,
            key: Some(KeptKey {
                secret: hex::encode(key.secret()),
                cert: hex::encode(key.cert_bytes()),
            }),
        };
        let code = secret(32)?;
        let mut codes = self
            .codes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        codes.retain(|_, code| code.expires_ms > now_ms);
        if codes.len() >= MAX_CODES {
            return Err(io::Error::other("too many authorizations in flight"));
        }
        codes.insert(
            digest(&code),
            Code {
                grant,
                client_id: client.id.clone(),
                redirect_uri,
                challenge,
                expires_ms: now_ms.saturating_add(millis(CODE_LIFETIME)),
            },
        );
        drop(codes);
        Ok(code)
    }

    /// Record `grant`, from a redeemed code, and issue its first tokens.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn redeem(&self, grant: Grant, now_ms: u64) -> io::Result<Result<Issued, TokenError>> {
        let id = grant.id.clone();
        self.change(|state| state.grants.insert(grant.id.clone(), grant))?;
        self.issue(&id, now_ms)
    }

    /// Redeem `code` once. The caller checks the PKCE verifier against the
    /// code's challenge before tokens are issued ([`Self::issue`]).
    #[must_use]
    pub fn take_code(&self, code: &str, now_ms: u64) -> Option<Code> {
        self.codes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&digest(code))
            .filter(|code| code.expires_ms > now_ms)
    }

    /// A fresh access and refresh token for the live grant `grant`.
    ///
    /// # Errors
    /// The grant was revoked or is unknown, or the store could not be written.
    pub fn issue(&self, grant: &str, now_ms: u64) -> io::Result<Result<Issued, TokenError>> {
        let access = format!("tilo_at_{}", secret(32)?);
        let refresh = format!("tilo_rt_{}", secret(32)?);
        self.change(|state| {
            let Some(grant) = state
                .grants
                .get(grant)
                .filter(|grant| grant.usable(now_ms))
                .cloned()
            else {
                return Err(TokenError::InvalidGrant);
            };
            // A key's token stays past its expiry, so the server says it
            // expired rather than that it never was a key.
            state
                .tokens
                .retain(|_, token| token.kind == TokenKind::Key || token.expires_ms > now_ms);
            for (secret, kind, lifetime) in [
                (&access, TokenKind::Access, ACCESS_LIFETIME),
                (&refresh, TokenKind::Refresh, REFRESH_LIFETIME),
            ] {
                state.tokens.insert(
                    digest(secret),
                    Token {
                        kind,
                        grant: grant.id.clone(),
                        expires_ms: now_ms.saturating_add(millis(lifetime)),
                    },
                );
            }
            Ok(Issued {
                access: access.clone(),
                refresh: refresh.clone(),
                grant,
            })
        })
    }

    /// Exchange `refresh` for a new pair. The refresh token is spent: a
    /// public client's refresh tokens rotate, as OAuth 2.1 asks.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn refresh(
        &self,
        refresh: &str,
        client_id: &str,
        now_ms: u64,
    ) -> io::Result<Result<Issued, TokenError>> {
        let spent = self.change(|state| {
            let key = digest(refresh);
            let token = state.tokens.get(&key).cloned();
            let grant = token
                .as_ref()
                .filter(|token| token.kind == TokenKind::Refresh && token.expires_ms > now_ms)
                .and_then(|token| state.grants.get(&token.grant))
                .filter(|grant| grant.usable(now_ms))
                .cloned();
            match grant {
                Some(grant) if grant.client_id == client_id => {
                    state.tokens.remove(&key);
                    Ok(grant.id)
                }
                Some(_) => Err(TokenError::InvalidClient),
                None => Err(TokenError::InvalidGrant),
            }
        })?;
        match spent {
            Ok(grant) => self.issue(&grant, now_ms),
            Err(error) => Ok(Err(error)),
        }
    }

    /// The live grant the access token or key `token` was issued under,
    /// noting that it was used now. A minted grant answers only while the
    /// grant that minted it is live.
    ///
    /// # Errors
    /// Why the token reaches nothing: unknown, revoked or expired.
    pub fn access(&self, token: &str, now_ms: u64) -> Result<Grant, Denied> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(held) = state
            .tokens
            .get(&digest(token))
            .filter(|token| token.kind != TokenKind::Refresh)
            .cloned()
        else {
            drop(state);
            return self.carried(token, now_ms);
        };
        let (id, expires_ms) = (held.grant.clone(), held.expires_ms);
        let grant = state.grants.get(&id).ok_or(Denied::Unknown)?;
        if let Some(denied) = Denied::of(grant, now_ms) {
            return Err(denied);
        }
        if expires_ms <= now_ms {
            return Err(Denied::Expired {
                at_ms: Some(expires_ms),
            });
        }
        if let Some(parent) = grant.parent() {
            let parent = state.grants.get(parent).ok_or(Denied::Unknown)?;
            if let Some(denied) = Denied::of(parent, now_ms) {
                return Err(denied);
            }
        }
        let grant = state.grants.get_mut(&id).ok_or(Denied::Unknown)?;
        grant.last_used_ms = Some(now_ms);
        let used = grant.clone();
        if now_ms.saturating_sub(self.saved_ms.load(Ordering::Relaxed)) >= LAST_USED_SAVE_MS
            && let Err(error) = self.save(&state)
        {
            tracing::warn!(%error, "could not keep when a key was last used");
        }
        drop(state);
        Ok(used)
    }

    /// The grant a key carries itself, for a token this store never saw: the
    /// key is the grant the person approved, so every API server reads it
    /// without holding anything another API server would have to share.
    ///
    /// Reading it keeps nothing. Nothing but the token vouches for such a
    /// key, and a root anybody makes certifies a key anybody makes, so it
    /// is kept only once a server it is for has let its person in
    /// ([`Self::keep_carried`]). Revoking a kept one here records it under
    /// the same id every API server derives from it, so a store that
    /// revoked one refuses it from then on.
    fn carried(&self, token: &str, now_ms: u64) -> Result<Grant, Denied> {
        let (name, key) = carried_key(token).ok_or(Denied::Unknown)?;
        let id = grant_id_of(&key);
        let kept = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.grants.get_mut(&id) {
                Some(known) => {
                    if let Some(denied) = Denied::of(known, now_ms) {
                        return Err(denied);
                    }
                    known.last_used_ms = Some(now_ms);
                    Some(known.clone())
                }
                None => None,
            }
        };
        if let Some(kept) = kept {
            return Ok(kept);
        }
        if !key.holds(now_ms.saturating_mul(1_000_000)) {
            return Err(Denied::Expired {
                at_ms: expiry_ms(&key),
            });
        }
        let grant = Grant {
            id,
            person: key.person(),
            client_id: String::new(),
            client_name: name,
            scope: scope_of(&key),
            kind: GrantKind::Key,
            expires_ms: expiry_ms(&key),
            browser_pages: key.browser_pages(),
            created_ms: key.cert().issued_at_ns / 1_000_000,
            last_used_ms: Some(now_ms),
            created_by: None,
            revoked_ms: None,
            key: Some(KeptKey {
                secret: hex::encode(key.secret()),
                cert: hex::encode(key.cert_bytes()),
            }),
        };
        Ok(grant)
    }

    /// Keep `grant`, a key this store did not issue and read from its token
    /// alone ([`Self::access`]), now that a server it is for has let its
    /// person in. From here on it is listed among that person's keys and
    /// revoked here like one made here.
    ///
    /// A person's kept keys of this kind are bounded
    /// ([`MAX_CARRIED_PER_PERSON`]). Past the bound the one used longest ago
    /// gives way, which loses nothing: a key is read from its token again.
    /// A revoked one stays, so it stays refused, and so does one that
    /// minted a key or made a webhook, which read as it.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn keep_carried(&self, grant: &Grant) -> io::Result<()> {
        if self.read(|state| state.grants.contains_key(&grant.id)) {
            return Ok(());
        }
        self.change(|state| {
            state
                .grants
                .entry(grant.id.clone())
                .or_insert_with(|| grant.clone());
            let mut carried: Vec<(u64, String)> = state
                .grants
                .values()
                .filter(|kept| kept.person == grant.person && is_carried(state, kept))
                .map(|kept| {
                    (
                        kept.last_used_ms.unwrap_or(kept.created_ms),
                        kept.id.clone(),
                    )
                })
                .collect();
            carried.sort();
            let over = carried.len().saturating_sub(MAX_CARRIED_PER_PERSON);
            let gone: Vec<String> = carried
                .into_iter()
                .map(|(_, id)| id)
                .filter(|id| !is_read_as(state, id))
                .take(over)
                .collect();
            for id in gone {
                state.grants.remove(&id);
            }
        })
    }

    /// Record a grant the person approved as an API key on their device
    /// `created_by`, or one a grant minted (`kind`), named `name`, with `key`
    /// and its certificate, and issue its one bearer token, lasting as long
    /// as the certificate. A minted grant's parent must be live. The token
    /// is answered here once; only its digest is kept.
    ///
    /// # Errors
    /// The store could not be written, or the parent is not live.
    pub fn resource_key(
        &self,
        key: &GrantKey,
        name: &str,
        kind: GrantKind,
        created_by: Option<String>,
        now_ms: u64,
    ) -> io::Result<(Grant, String)> {
        let token = key_token(name, key)?;
        let grant = Grant {
            id: grant_id_of(key),
            person: key.person(),
            client_id: String::new(),
            client_name: name.to_owned(),
            scope: scope_of(key),
            kind,
            expires_ms: expiry_ms(key),
            browser_pages: key.browser_pages(),
            created_ms: now_ms,
            last_used_ms: None,
            created_by,
            revoked_ms: None,
            key: Some(KeptKey {
                secret: hex::encode(key.secret()),
                cert: hex::encode(key.cert_bytes()),
            }),
        };
        self.change(|state| {
            if let Some(parent) = grant.parent()
                && !state
                    .grants
                    .get(parent)
                    .is_some_and(|parent| parent.usable(now_ms))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the grant that would mint this one is not live",
                ));
            }
            state.grants.insert(grant.id.clone(), grant.clone());
            state.tokens.insert(
                digest(&token),
                Token {
                    kind: TokenKind::Key,
                    grant: grant.id.clone(),
                    expires_ms: grant.expires_ms.unwrap_or(u64::MAX),
                },
            );
            Ok(())
        })??;
        Ok((grant, token))
    }

    /// The live grant `id`.
    #[must_use]
    pub fn grant(&self, id: &str) -> Option<Grant> {
        self.read(|state| state.grants.get(id).filter(|grant| grant.live()).cloned())
    }

    /// Whether the grant `id` was revoked here.
    #[must_use]
    pub fn revoked(&self, id: &str) -> bool {
        self.read(|state| state.grants.get(id).is_some_and(|grant| !grant.live()))
    }

    /// The live grants `parent` minted, newest first.
    #[must_use]
    pub fn minted_by(&self, parent: &str) -> Vec<Grant> {
        let mut grants: Vec<Grant> = self.read(|state| {
            state
                .grants
                .values()
                .filter(|grant| grant.parent() == Some(parent) && grant.live())
                .cloned()
                .collect()
        });
        grants.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(a.id.cmp(&b.id)));
        grants
    }

    /// Forget one token (RFC 7009). Revoking a refresh token revokes the
    /// whole grant, as the RFC allows: the client asked to be forgotten.
    /// Answers the grants that were revoked: the grant and what it minted.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn revoke_token(
        &self,
        token: &str,
        client_id: &str,
        now_ms: u64,
    ) -> io::Result<Vec<String>> {
        self.change(|state| {
            let key = digest(token);
            let Some(token) = state.tokens.get(&key).cloned() else {
                return Vec::new();
            };
            if state
                .grants
                .get(&token.grant)
                .is_none_or(|grant| grant.client_id != client_id)
            {
                return Vec::new();
            }
            state.tokens.remove(&key);
            if token.kind == TokenKind::Refresh {
                return revoke_with_minted(state, &token.grant, now_ms);
            }
            Vec::new()
        })
    }

    /// Revoke `person`'s grant `grant` and every grant it minted. Answers
    /// the grants revoked, none when it was not theirs and live.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn revoke_grant(&self, person: &str, grant: &str, now_ms: u64) -> io::Result<Vec<String>> {
        self.change(|state| {
            if !state
                .grants
                .get(grant)
                .is_some_and(|found| found.person == person && found.live())
            {
                return Vec::new();
            }
            revoke_with_minted(state, grant, now_ms)
        })
    }

    /// `person`'s live grants, newest first.
    #[must_use]
    pub fn grants_of(&self, person: &str) -> Vec<Grant> {
        let mut grants: Vec<Grant> = self.read(|state| {
            state
                .grants
                .values()
                .filter(|grant| grant.person == person && grant.live())
                .cloned()
                .collect()
        });
        grants.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(a.id.cmp(&b.id)));
        grants
    }

    /// The number of `person`'s next agent session, counting from one.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn next_run(&self, person: &str) -> io::Result<u64> {
        // Counted for a person this store keeps a grant of. A key read from
        // its token alone, whose person no server has let in yet, leaves
        // nothing here: its first is its person's first.
        if !self.read(|state| state.grants.values().any(|grant| grant.person == person)) {
            return Ok(1);
        }
        self.change(|state| {
            let count = state.runs.entry(person.to_owned()).or_default();
            *count = count.saturating_add(1);
            *count
        })
    }
}

/// The prefix of a key that carries its own grant: `tilo_sk_…`.
pub const KEY_PREFIX: &str = "tilo_sk_";

/// The key a person approved, as the one bearer token a key is: its name,
/// its secret and the person's certificate, so every API server reads the
/// grant without a store one API server would have to share with it.
///
/// # Errors
/// The key cannot be encoded.
pub fn key_token(name: &str, key: &GrantKey) -> io::Result<String> {
    use base64::Engine as _;
    let carried = KeyToken {
        name: name.to_owned(),
        secret: hex::encode(key.secret()),
        cert: hex::encode(key.cert_bytes()),
    };
    let json = serde_json::to_vec(&carried).map_err(io::Error::other)?;
    Ok(format!(
        "{KEY_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    ))
}

/// The grant `token` carries: its name and the key with its certificate.
pub(crate) fn carried_key(token: &str) -> Option<(String, GrantKey)> {
    use base64::Engine as _;
    let encoded = token.strip_prefix(KEY_PREFIX)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    let carried: KeyToken = serde_json::from_slice(&json).ok()?;
    let key = GrantKey::from_kept(
        &hex::decode(&carried.secret).ok()?,
        &hex::decode(&carried.cert).ok()?,
    )
    .ok()?;
    Some((carried.name, key))
}

/// The grant `key` is, by the same id every API server derives from it: its
/// certificate's device key, so a grant recorded or revoked at one API
/// server is the same grant at another.
#[must_use]
pub fn grant_id_of(key: &GrantKey) -> String {
    collab_types::workspace_grant::WorkspaceGrant::id_of(&key.cert().device_public_key)
}

/// A key as its token carries it.
#[derive(Serialize, Deserialize)]
struct KeyToken {
    /// What the person approved the key for, as it was asked ("Visa backend").
    name: String,
    /// The key's 32 secret bytes, hex.
    secret: String,
    /// The certificate's canonical encoding, hex.
    cert: String,
}

/// `bytes` of fresh randomness, base64url without padding.
///
/// # Errors
/// The system's random source failed.
pub fn secret(bytes: usize) -> io::Result<String> {
    use base64::Engine as _;
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buffer))
}

pub(crate) fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// The PKCE S256 transform (RFC 7636): `BASE64URL(SHA256(verifier))`.
#[must_use]
pub fn challenge_of(verifier: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
