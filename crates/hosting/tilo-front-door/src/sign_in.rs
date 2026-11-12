//! Who is at the browser: the person a device of theirs signed in.
//!
//! The door keeps no accounts. It asks the person's Tilo for a
//! [`SignInProof`] of its challenge and a certificate for the grant's key:
//! it sends the browser to the person's Tilo on the web (the `web_entry`),
//! which holds a certified device key and the person's root, with
//!
//! ```text
//! <web_entry>/sign-in?audience=<issuer>&challenge=<hex>&return=<issuer>/sign-in&pending=<id>
//!     &client=<name>&grant_key=<hex>&grant_scope=<all|workspace>&grant_expires=<unix ms|never>
//!     [&grant_kind=server_key]
//! ```
//!
//! and the page there, once the person agrees, posts a form back to
//! `return` with `pending`, `proof` and `grant` (each a canonical encoding,
//! base64url), or `pending` and `decision=deny`. The door checks the proof
//! ([`SignInProof::verify`]) against the person's own revocation list, and
//! the certificate against the key and the scope it asked for — the person
//! may narrow a grant to one of their workspaces, never widen one — and
//! gives the browser a session cookie for its grants page. The grant the
//! door records reaches what the certificate says.
//!
//! A challenge belongs to one authorization in progress and is spent by
//! the first proof that answers it.

use std::{collections::HashMap, sync::Mutex};
use tilo_user_identity::{RevocationList, SignInProof};

/// The browser session cookie's name. `__Host-` makes browsers refuse it
/// unless it is `Secure`, has no `Domain` and is set for `/`, so no other
/// origin under the same site can plant or read it.
pub const COOKIE: &str = "__Host-tilo-door";
/// How long a browser stays signed in.
pub const BROWSER_SESSION_MS: u64 = 12 * 60 * 60 * 1000;
const MAX_BROWSER_SESSIONS: usize = 16_384;

#[derive(Clone)]
struct Signed {
    person: String,
    expires_ms: u64,
}

/// The browsers signed in, by their cookie.
#[derive(Default)]
pub struct Browsers {
    sessions: Mutex<HashMap<String, Signed>>,
}

impl Browsers {
    /// Sign `person` in: the cookie value for their browser.
    ///
    /// # Errors
    /// The system's random source failed.
    pub fn sign_in(&self, person: String, now_ms: u64) -> std::io::Result<String> {
        let cookie = crate::store::secret(32)?;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.retain(|_, signed| signed.expires_ms > now_ms);
        if sessions.len() >= MAX_BROWSER_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, signed)| signed.expires_ms)
                .map(|(key, _)| key.clone())
        {
            sessions.remove(&oldest);
        }
        sessions.insert(
            cookie.clone(),
            Signed {
                person,
                expires_ms: now_ms.saturating_add(BROWSER_SESSION_MS),
            },
        );
        drop(sessions);
        Ok(cookie)
    }

    /// The person signed in at the browser that sent `headers`.
    pub fn person(&self, headers: &hyper::HeaderMap, now_ms: u64) -> Option<String> {
        let cookie = cookie(headers)?;
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cookie)
            .filter(|signed| signed.expires_ms > now_ms)
            .map(|signed| signed.person.clone())
    }

    /// Sign the browser that sent `headers` out.
    pub fn sign_out(&self, headers: &hyper::HeaderMap) {
        if let Some(cookie) = cookie(headers) {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&cookie);
        }
    }
}

fn cookie(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get_all(hyper::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_owned())
}

/// The `Set-Cookie` value that signs a browser in.
#[must_use]
pub fn set_cookie(value: &str) -> String {
    format!(
        "{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
        BROWSER_SESSION_MS / 1000
    )
}

/// A posted proof, decoded: base64url of its canonical encoding.
#[must_use]
pub fn decode_proof(text: &str) -> Option<SignInProof> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim().trim_end_matches('='))
        .ok()?;
    tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok()
}

/// A posted grant certificate, decoded: base64url of its canonical encoding.
#[must_use]
pub fn decode_cert(text: &str) -> Option<tilo_user_identity::DeviceCert> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim().trim_end_matches('='))
        .ok()?;
    tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok()
}

/// `cert`, encoded as a sign-in page posts it.
#[must_use]
pub fn encode_cert(cert: &tilo_user_identity::DeviceCert) -> Option<String> {
    use base64::Engine as _;
    let bytes = tilo_user_identity::canonical::canonical_bytes(cert).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// `proof`, encoded as a sign-in page posts it; `None` if it has no
/// canonical encoding.
#[must_use]
pub fn encode_proof(proof: &SignInProof) -> Option<String> {
    use base64::Engine as _;
    let bytes = tilo_user_identity::canonical::canonical_bytes(proof).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Check `proof` answers `challenge` for the door at `issuer`, under the
/// person's own `revocations`. Answers the person, hex.
///
/// # Errors
/// Why the proof does not sign anyone in.
pub fn verify(
    proof: &SignInProof,
    issuer: &str,
    challenge: &[u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<String, tilo_user_identity::SignInError> {
    proof
        .verify(issuer, challenge, revocations, now_ns)
        .map(hex::encode)
}
