//! `Idempotency-Key`: a retried mutating call answers what the first one
//! answered, and never runs anything twice.
//!
//! The server remembers each key holder's answers for a day, by the key the
//! caller sent and a digest of the request it came with. An action's
//! invocation is derived from the key as well ([`invocation`]), so the host
//! itself runs it once even when this memory is gone (a restart). A call
//! that is still pending is not remembered: retrying it asks about the same
//! invocation again, which is how a caller awaits it.

use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The header a caller names its attempt with.
pub const HEADER: &str = "idempotency-key";
/// How long an answer is remembered.
const KEPT: Duration = Duration::from_secs(24 * 60 * 60);
/// The most answers remembered at once, all key holders together.
const MAX_KEPT: usize = 100_000;
const MAX_KEY_CHARS: usize = 255;

/// An answer as it is replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub status: u16,
    pub body: serde_json::Value,
    pub location: Option<String>,
}

/// What a request with an idempotency key may do.
#[derive(Debug, PartialEq, Eq)]
pub enum Begun {
    /// Nothing ran under this key yet: run it, then [`Remembered::finish`].
    Fresh,
    /// It ran: answer this again.
    Replay(Stored),
    /// The key came with another request, or with this one still running.
    Conflict(&'static str),
}

struct Entry {
    request: [u8; 32],
    at: Instant,
    answer: Option<Stored>,
}

/// The answers remembered, per key holder and key.
#[derive(Default)]
pub struct Remembered {
    entries: Mutex<HashMap<(String, String), Entry>>,
}

/// `key` when it is one: 1 to 255 printable characters.
#[must_use]
pub fn valid(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= MAX_KEY_CHARS
        && key.chars().all(|character| !character.is_control())
}

/// The digest a request is told apart by: its method, path and body.
#[must_use]
pub fn request_digest(method: &str, path: &str, body: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in [method.as_bytes(), path.as_bytes(), body] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

/// The invocation an action asked with `key` by `holder` runs as: the same
/// for every retry, and another holder's same key never collides with it.
#[must_use]
pub fn invocation(holder: &str, key: &str) -> uuid::Uuid {
    let mut hash = Sha256::new();
    hash.update(b"tilo-api idempotent invocation\0");
    hash.update(holder.as_bytes());
    hash.update([0]);
    hash.update(key.as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(digest.get(..16).unwrap_or(&[0; 16]));
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

impl Remembered {
    /// Begin `request` under `holder`'s `key`.
    pub fn begin(&self, holder: &str, key: &str, request: [u8; 32]) -> Begun {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if entries.len() >= MAX_KEPT {
            entries.retain(|_, entry| now.duration_since(entry.at) < KEPT);
        }
        let id = (holder.to_owned(), key.to_owned());
        match entries.get(&id) {
            Some(entry) if now.duration_since(entry.at) < KEPT => {
                if entry.request != request {
                    return Begun::Conflict("this Idempotency-Key was sent with another request");
                }
                return entry.answer.clone().map_or(
                    Begun::Conflict("a request with this Idempotency-Key is still running"),
                    Begun::Replay,
                );
            }
            _ => {}
        }
        if entries.len() >= MAX_KEPT {
            return Begun::Fresh;
        }
        entries.insert(
            id,
            Entry {
                request,
                at: now,
                answer: None,
            },
        );
        Begun::Fresh
    }

    /// The request under `holder`'s `key` answered `answer`; `None` forgets
    /// it, for an answer a retry must not replay (pending, or a refusal the
    /// same request may not meet again).
    pub fn finish(&self, holder: &str, key: &str, answer: Option<Stored>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = (holder.to_owned(), key.to_owned());
        match answer {
            Some(answer) => {
                if let Some(entry) = entries.get_mut(&id) {
                    entry.answer = Some(answer);
                }
            }
            None => {
                entries.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_retry_replays_and_another_request_under_the_key_conflicts() {
        let remembered = Remembered::default();
        let request = request_digest("POST", "/v1/workspaces", b"{\"name\":\"a\"}");
        assert_eq!(remembered.begin("g", "k", request), Begun::Fresh);
        assert!(matches!(
            remembered.begin("g", "k", request),
            Begun::Conflict(_)
        ));
        let stored = Stored {
            status: 201,
            body: json!({"id": "w"}),
            location: None,
        };
        remembered.finish("g", "k", Some(stored.clone()));
        assert_eq!(remembered.begin("g", "k", request), Begun::Replay(stored));
        let other = request_digest("POST", "/v1/workspaces", b"{\"name\":\"b\"}");
        assert!(matches!(
            remembered.begin("g", "k", other),
            Begun::Conflict(_)
        ));
        assert_eq!(remembered.begin("h", "k", other), Begun::Fresh);
    }

    #[test]
    fn a_pending_answer_is_forgotten_so_a_retry_asks_again() {
        let remembered = Remembered::default();
        let request = request_digest("POST", "/x", b"{}");
        assert_eq!(remembered.begin("g", "k", request), Begun::Fresh);
        remembered.finish("g", "k", None);
        assert_eq!(remembered.begin("g", "k", request), Begun::Fresh);
    }

    #[test]
    fn the_invocation_is_the_keys_and_the_holders() {
        assert_eq!(invocation("g", "k"), invocation("g", "k"));
        assert_ne!(invocation("g", "k"), invocation("h", "k"));
        assert!(valid("retry-1") && !valid("") && !valid("a\nb"));
    }
}
