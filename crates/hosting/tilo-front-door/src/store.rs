//! What the door remembers: the clients that registered, the grants people
//! gave them and the tokens issued under each grant.
//!
//! Tokens are bearer secrets, so only their SHA-256 digests are kept: a copy
//! of the file names no credential a caller could present. Authorization
//! codes live in memory only; they are redeemed within a minute, and a
//! restart simply makes the client ask again.
//!
//! A grant is the person's decision, and revoking it is final: its tokens stop
//! working at once, its refresh token cannot mint new ones, and the sessions
//! opened under it are closed by the door ([`crate::Door::revoke_grant`]).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io, path::PathBuf, sync::Mutex, time::Duration};

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

/// A person's consent: this client, acting for this person, with this scope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    /// The person's identity root key, hex.
    pub person: String,
    pub client_id: String,
    /// What the person saw the client called when they agreed.
    pub client_name: String,
    pub scope: Scope,
    pub created_ms: u64,
    #[serde(default)]
    pub revoked_ms: Option<u64>,
}

impl Grant {
    #[must_use]
    pub fn live(&self) -> bool {
        self.revoked_ms.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TokenKind {
    Access,
    Refresh,
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
    /// How many agent runs each person has started through the door, so the
    /// next one is numbered after them ("Claude Code #3").
    runs: HashMap<String, u64>,
}

/// The door's memory, kept in one private file when it has a path.
pub struct Store {
    path: Option<PathBuf>,
    state: Mutex<State>,
    codes: Mutex<HashMap<String, Code>>,
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
        })
    }

    fn change<T>(&self, change: impl FnOnce(&mut State) -> T) -> io::Result<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let answer = change(&mut state);
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&*state).map_err(io::Error::other)?;
            tilo_files::native::private::write_atomic(path, &bytes)?;
        }
        // Held until the file is written: two changes writing their
        // snapshots unlocked could leave the older one on disk.
        drop(state);
        Ok(answer)
    }

    fn read<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(
            &self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
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

    /// Record the person's consent and the code the client redeems for it.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn consent(
        &self,
        person: &str,
        client: &Client,
        scope: Scope,
        redirect_uri: String,
        challenge: String,
        now_ms: u64,
    ) -> io::Result<String> {
        let grant = Grant {
            id: format!("grant-{}", secret(12)?),
            person: person.to_owned(),
            client_id: client.id.clone(),
            client_name: client.name.clone(),
            scope,
            created_ms: now_ms,
            revoked_ms: None,
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
                .filter(|grant| grant.live())
                .cloned()
            else {
                return Err(TokenError::InvalidGrant);
            };
            state.tokens.retain(|_, token| token.expires_ms > now_ms);
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
                .filter(|grant| grant.live())
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

    /// The live grant the access token `token` was issued under.
    #[must_use]
    pub fn access(&self, token: &str, now_ms: u64) -> Option<Grant> {
        self.read(|state| {
            let token = state.tokens.get(&digest(token))?;
            if token.kind != TokenKind::Access || token.expires_ms <= now_ms {
                return None;
            }
            state
                .grants
                .get(&token.grant)
                .filter(|grant| grant.live())
                .cloned()
        })
    }

    /// Forget one token (RFC 7009). Revoking a refresh token revokes the
    /// whole grant, as the RFC allows: the client asked to be forgotten.
    /// Answers the grant that was revoked, if any.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn revoke_token(
        &self,
        token: &str,
        client_id: &str,
        now_ms: u64,
    ) -> io::Result<Option<String>> {
        self.change(|state| {
            let key = digest(token);
            let token = state.tokens.get(&key).cloned()?;
            let grant = state.grants.get_mut(&token.grant)?;
            if grant.client_id != client_id {
                return None;
            }
            state.tokens.remove(&key);
            if token.kind == TokenKind::Refresh {
                grant.revoked_ms.get_or_insert(now_ms);
                let id = grant.id.clone();
                state.tokens.retain(|_, token| token.grant != id);
                return Some(id);
            }
            None
        })
    }

    /// Revoke `person`'s grant `grant`. Answers whether it was theirs and live.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn revoke_grant(&self, person: &str, grant: &str, now_ms: u64) -> io::Result<bool> {
        self.change(|state| {
            let Some(found) = state
                .grants
                .get_mut(grant)
                .filter(|found| found.person == person && found.live())
            else {
                return false;
            };
            found.revoked_ms = Some(now_ms);
            state.tokens.retain(|_, token| token.grant != grant);
            true
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

    /// The number of `person`'s next agent run, counting from one.
    ///
    /// # Errors
    /// The store could not be written.
    pub fn next_run(&self, person: &str) -> io::Result<u64> {
        self.change(|state| {
            let count = state.runs.entry(person.to_owned()).or_default();
            *count = count.saturating_add(1);
            *count
        })
    }
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

fn digest(secret: &str) -> String {
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
