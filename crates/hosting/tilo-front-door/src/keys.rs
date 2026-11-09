//! API keys: grants a person approves for their own backend or script,
//! without an OAuth client.
//!
//! A key is a grant like any agent's: the door makes its key, and the
//! person's own Tilo signs them in and certifies it (the same `/sign-in`
//! answer an agent's authorization gets). What comes back is one bearer
//! token, `tilo_sk_…`, lasting as long as the certificate, instead of a code
//! for a client. Scope checks, expiry, History attribution and revocation at
//! `/grants` are the grant's.
//!
//! Where the person asks matters for where the key goes:
//!
//! - **From the grants page** (`POST /grants/keys`): the page they return to
//!   shows the key once.
//! - **From a terminal** (`tilo keys create`): the terminal asks
//!   `POST /keys/new` and gets an approval link and a poll secret. It shows
//!   the link and polls `POST /keys/poll` until the person approves, then gets
//!   the key. The browser only says it was approved: the key goes only to the
//!   poller.

use crate::{
    Door, Purpose, constant_time_eq, html, json_reply, lock, now_ms, oauth, pages, redirect,
    sign_in, store, text,
};
use hyper::StatusCode;
use serde_json::{Value, json};
use std::collections::HashMap;
use tilo_user_identity::AgentScope;

pub const NEW_PATH: &str = "/keys/new";
pub const POLL_PATH: &str = "/keys/poll";
pub const APPROVE_PREFIX: &str = "/keys/approve/";
/// The most characters a key's name may be.
pub const MAX_NAME_CHARS: usize = 80;
/// How long an approved key waits for its poller.
const APPROVAL_LIFETIME_MS: u64 = 15 * 60 * 1000;
/// How often a poller should ask, in seconds.
pub const POLL_INTERVAL_S: u64 = 2;
const MAX_APPROVALS: usize = 4_096;

/// How the person answered a key a poller waits for.
pub(crate) struct Approval {
    outcome: Outcome,
    expires_ms: u64,
}

/// How a key someone waits for went, as [`Door::key_answer`] tells it.
pub(crate) enum Answer {
    Approved {
        token: String,
        grant: Box<store::Grant>,
    },
    Declined,
    /// The person has not answered yet; they approve at `ask`.
    Waiting {
        ask: String,
    },
    /// Nothing waits under that poll secret.
    Expired,
}

enum Outcome {
    Approved {
        token: String,
        grant: Box<store::Grant>,
    },
    Declined,
}

/// A key's name, as asked: 1 to [`MAX_NAME_CHARS`] printable characters.
fn name_of(asked: Option<&str>) -> Result<String, &'static str> {
    let name = asked.map(str::trim).unwrap_or_default();
    if name.is_empty() {
        return Err("name the key: what will use it, like \"Visa backend\"");
    }
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err("a key's name is 1 to 80 printable characters");
    }
    Ok(name.to_owned())
}

/// The scope asked: one workspace by its id, else all of them.
fn scope_of(workspace: Option<&str>) -> Result<AgentScope, &'static str> {
    match workspace.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(AgentScope::AllWorkspaces),
        Some(id) => AgentScope::parse(id).ok_or("`workspace` is a workspace's id, a UUID"),
    }
}

fn error(status: StatusCode, code: &str, message: &str, next: &str) -> crate::Reply {
    json_reply(
        status,
        &json!({"error": {"code": code, "message": message, "next": next}}),
    )
}

impl Door {
    /// The sign-in for a new key, ready to start.
    fn key_sign_in(
        &self,
        name: String,
        scope: AgentScope,
        poll: Option<String>,
    ) -> Result<(String, String), &'static str> {
        let mut challenge = [0u8; 32];
        let (Ok(()), Ok(id), Ok(key)) = (
            getrandom::fill(&mut challenge),
            store::secret(24),
            crate::grant_key::GrantKey::generate(),
        ) else {
            return Err("no randomness is available; try again shortly");
        };
        let shown = format!("API key \u{201c}{name}\u{201d}");
        let location = self.begin(
            Purpose::Key { name, poll },
            &shown,
            scope,
            (id.clone(), challenge, key),
            now_ms(),
        )?;
        Ok((id, location))
    }

    /// Ask the person for a key called `name`, reaching `workspace` or all
    /// of them: the poll secret that collects it ([`Self::key_answer`]), and
    /// the link where the person approves it.
    pub(crate) fn request_key(
        &self,
        name: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<(String, String), &'static str> {
        let name = name_of(name)?;
        let scope = scope_of(workspace)?;
        let poll = store::secret(32).map_err(|_| "no randomness is available; try again")?;
        let (id, _) = self.key_sign_in(name, scope, Some(store::digest(&poll)))?;
        Ok((poll, format!("{}{APPROVE_PREFIX}{id}", self.config.issuer)))
    }

    /// How the key `poll` collects went. An approved key is answered once.
    pub(crate) fn key_answer(&self, poll: &str) -> Answer {
        let digest = store::digest(poll);
        let now = now_ms();
        let answered = {
            let mut approved = lock(&self.approved_keys);
            approved.retain(|_, approval| approval.expires_ms > now);
            approved.remove(&digest)
        };
        if let Some(approval) = answered {
            return match approval.outcome {
                Outcome::Approved { token, grant } => Answer::Approved { token, grant },
                Outcome::Declined => Answer::Declined,
            };
        }
        let waiting = lock(&self.pending).iter().find_map(|(id, pending)| {
            (pending.expires_ms > now
                && matches!(&pending.purpose, Purpose::Key { poll: Some(waiting), .. }
                    if constant_time_eq(waiting.as_bytes(), digest.as_bytes())))
            .then(|| format!("{}{APPROVE_PREFIX}{id}", self.config.issuer))
        });
        match waiting {
            Some(ask) => Answer::Waiting { ask },
            None => Answer::Expired,
        }
    }

    /// `POST /keys/new` `{name, workspace?}`: a key a terminal waits for.
    pub(crate) fn new_key(&self, body: &[u8]) -> crate::Reply {
        let asked: Value = serde_json::from_slice(body).unwrap_or_default();
        let field = |name: &str| asked.get(name).and_then(Value::as_str);
        match self.request_key(field("name"), field("workspace")) {
            Ok((poll, approve_url)) => json_reply(
                StatusCode::CREATED,
                &json!({
                    "approve_url": approve_url,
                    "poll": poll,
                    "interval": POLL_INTERVAL_S,
                    "expires_in": crate::PENDING_LIFETIME_MS / 1000,
                }),
            ),
            Err(why) => error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                why,
                "send {\"name\": \"…\"}, and \"workspace\": \"<id>\" for a key reaching one workspace",
            ),
        }
    }

    /// `GET /keys/approve/<id>`: on to the person's Tilo, which asks them.
    pub(crate) fn approve_key(&self, id: &str) -> crate::Reply {
        let now = now_ms();
        let location = lock(&self.pending)
            .get(id)
            .filter(|pending| {
                pending.expires_ms > now && matches!(pending.purpose, Purpose::Key { .. })
            })
            .map(|pending| pending.location.clone());
        match location {
            Some(location) => redirect(&location),
            None => html(
                StatusCode::NOT_FOUND,
                &pages::message(
                    "This link expired",
                    "Ask for the key again: run `tilo keys create` once more.",
                ),
            ),
        }
    }

    /// `POST /keys/poll` `{poll}`: the key, once the person approved it.
    pub(crate) fn poll_key(&self, body: &[u8]) -> crate::Reply {
        let asked: Value = serde_json::from_slice(body).unwrap_or_default();
        let Some(poll) = asked.get("poll").and_then(Value::as_str) else {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "`poll` is required",
                "send the poll secret /keys/new answered",
            );
        };
        match self.key_answer(poll) {
            Answer::Approved { token, grant } => json_reply(
                StatusCode::OK,
                &json!({
                    "key": token,
                    "grant": grant.id,
                    "name": grant.client_name,
                    "scope": grant.scope.name(),
                    "expires_at_ms": grant.expires_ms,
                }),
            ),
            Answer::Declined => error(
                StatusCode::FORBIDDEN,
                "access_denied",
                "the person declined the key",
                "ask again with `tilo keys create` if that was a mistake",
            ),
            Answer::Waiting { .. } => {
                json_reply(StatusCode::ACCEPTED, &json!({"status": "pending"}))
            }
            Answer::Expired => error(
                StatusCode::NOT_FOUND,
                "expired",
                "no key waits for this poll secret: it expired or was already collected",
                "ask again with `tilo keys create`",
            ),
        }
    }

    /// `POST /grants/keys`: the grants page asks for a key.
    pub(crate) fn new_key_from_page(
        &self,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> crate::Reply {
        if self.browsers.person(headers, now_ms()).is_none() {
            return html(
                StatusCode::UNAUTHORIZED,
                &pages::message("Sign in first", "Your sign-in expired."),
            );
        }
        let form = oauth::form(body).unwrap_or_default();
        if !self.posted_from_grants_page(headers, &form) {
            return html(
                StatusCode::FORBIDDEN,
                &pages::message("Not allowed", "This did not come from your grants page."),
            );
        }
        let field = |name: &str| form.get(name).map(String::as_str);
        let (name, scope) = match (name_of(field("name")), scope_of(field("workspace"))) {
            (Ok(name), Ok(scope)) => (name, scope),
            (Err(why), _) | (_, Err(why)) => {
                return html(
                    StatusCode::BAD_REQUEST,
                    &pages::message("Key not created", why),
                );
            }
        };
        match self.key_sign_in(name, scope, None) {
            Ok((_, location)) => redirect(&location),
            Err(why) => text(StatusCode::SERVICE_UNAVAILABLE, why),
        }
    }

    /// The person's Tilo answered a key's sign-in.
    pub(crate) fn key_signed_in(
        &self,
        pending: &crate::Pending,
        name: &str,
        poll: Option<&str>,
        form: &HashMap<String, String>,
        now: u64,
    ) -> crate::Reply {
        if form.get("decision").map(String::as_str) == Some("deny") {
            if let Some(poll) = poll {
                self.answer_poller(poll, Outcome::Declined, now);
            }
            return html(
                StatusCode::OK,
                &pages::message("Key declined", "No key was made."),
            );
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
        let key = match self.check_answer(pending, &proof, cert, now) {
            Ok(key) => key,
            Err(reply) => return *reply,
        };
        let Ok((grant, token)) = self
            .store
            .record_key(&key, name, store::GrantKind::Key, now)
        else {
            return text(StatusCode::SERVICE_UNAVAILABLE, "keys are unavailable");
        };
        let mut reply = match poll {
            Some(poll) => {
                self.answer_poller(
                    poll,
                    Outcome::Approved {
                        token,
                        grant: Box::new(grant),
                    },
                    now,
                );
                html(
                    StatusCode::OK,
                    &pages::message(
                        "Key approved",
                        "Go back to your terminal: the key is there. You can revoke it on your grants page.",
                    ),
                )
            }
            None => html(StatusCode::OK, &pages::key_created(&grant, &token)),
        };
        if let Ok(cookie) = self.browsers.sign_in(key.person(), now)
            && let Ok(value) = hyper::header::HeaderValue::from_str(&sign_in::set_cookie(&cookie))
        {
            reply.headers_mut().insert(hyper::header::SET_COOKIE, value);
        }
        reply
    }

    fn answer_poller(&self, poll: &str, outcome: Outcome, now: u64) {
        let mut approved = lock(&self.approved_keys);
        approved.retain(|_, approval| approval.expires_ms > now);
        if approved.len() < MAX_APPROVALS {
            approved.insert(
                poll.to_owned(),
                Approval {
                    outcome,
                    expires_ms: now.saturating_add(APPROVAL_LIFETIME_MS),
                },
            );
        }
    }
}
