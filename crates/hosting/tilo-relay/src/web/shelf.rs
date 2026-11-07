//! The shelf a site keeps sealed invitations on: `/i/<handle>`.
//!
//! An invitation's link carries a handle and, in its fragment, the key that
//! opens what the handle names. The bytes themselves wait here: ciphertext a
//! site has no key for, until the invitation it holds lapses or the inviter
//! takes it back. Neither the key nor the invitation is ever written down;
//! the delete secret is kept as a digest, so what a site holds cannot be used
//! to read an invitation even by the site itself.

use std::collections::HashMap;
use std::sync::Mutex;

use hyper::{Method, Request, Response, StatusCode};
use tilo_relay_protocol::sealed_invitations::{
    Fetched, MAX_SEALED_BYTES, MAX_STORED_SECONDS, Stored, TakeBack,
};

use super::{Body, Web, body, json_status, reply, status};

/// The most invitations a site keeps at once.
const MAX_ENTRIES: usize = 4096;
/// The most sealed bytes a site keeps at once.
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Characters a delete secret is spelled in, as the inviter's device draws it.
const DELETE_CHARS: usize = 43;
/// Nanoseconds in a second, for the window an invitation may be kept for.
const NS: u64 = 1_000_000_000;

/// What a site keeps in place of an invitation.
struct Kept {
    /// The sealed signed invitation, base64url.
    sealed: String,
    /// When the invitation it holds lapses.
    expires_at_ns: u64,
    /// The digest of the secret that takes it back.
    delete: [u8; 32],
}

#[derive(Default)]
struct Inner {
    kept: HashMap<String, Kept>,
    bytes: usize,
}

impl Inner {
    /// Drop what has lapsed: a site keeps nothing past an invitation's life.
    fn sweep(&mut self, now_ns: u64) {
        self.kept.retain(|_, kept| {
            let live = kept.expires_at_ns > now_ns;
            if !live {
                self.bytes = self.bytes.saturating_sub(kept.sealed.len());
            }
            live
        });
    }
}

/// Why a site did not keep or hand back an invitation.
#[derive(Debug)]
pub(super) enum Refused {
    /// The request is not something the shelf can hold.
    Malformed(&'static str),
    /// Another invitation is kept under that handle.
    Taken,
    /// The shelf holds as much as it will.
    Full,
    /// Nothing is kept under that handle any more.
    Gone,
    /// The secret is not the one the invitation was kept with.
    Forbidden,
}

/// The invitations a site keeps, and nothing it can read.
#[derive(Default)]
pub(super) struct Shelf {
    inner: Mutex<Inner>,
}

impl Shelf {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keep `stored` under `handle`, unless the shelf cannot hold it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    pub(super) fn put(&self, handle: &str, stored: Stored, now_ns: u64) -> Result<(), Refused> {
        if stored.sealed.is_empty() || stored.sealed.len() > MAX_SEALED_BYTES {
            return Err(Refused::Malformed("a sealed invitation is at most 8 KiB"));
        }
        if !spelled(&stored.delete) {
            return Err(Refused::Malformed(
                "a delete secret is 43 base64url characters",
            ));
        }
        let longest = now_ns.saturating_add(MAX_STORED_SECONDS.saturating_mul(NS));
        if stored.expires_at_ns <= now_ns || stored.expires_at_ns > longest {
            return Err(Refused::Malformed(
                "an invitation is kept for its own life, at most 30 days",
            ));
        }
        let bytes = stored.sealed.len();
        let mut inner = self.inner();
        inner.sweep(now_ns);
        if inner.kept.contains_key(handle) {
            return Err(Refused::Taken);
        }
        if inner.kept.len() >= MAX_ENTRIES || inner.bytes.saturating_add(bytes) > MAX_BYTES {
            return Err(Refused::Full);
        }
        inner.bytes = inner.bytes.saturating_add(bytes);
        inner.kept.insert(
            handle.to_owned(),
            Kept {
                sealed: stored.sealed,
                expires_at_ns: stored.expires_at_ns,
                delete: digest(&stored.delete),
            },
        );
        Ok(())
    }

    /// The sealed invitation `handle` names, while it is still there.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    fn get(&self, handle: &str, now_ns: u64) -> Option<String> {
        let mut inner = self.inner();
        inner.sweep(now_ns);
        inner.kept.get(handle).map(|kept| kept.sealed.clone())
    }

    /// Take an invitation back, if `delete` is the secret it was kept with.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    fn take(&self, handle: &str, delete: &str, now_ns: u64) -> Result<(), Refused> {
        let mut inner = self.inner();
        inner.sweep(now_ns);
        let Some(kept) = inner.kept.get(handle) else {
            return Err(Refused::Gone);
        };
        if !same(&kept.delete, &digest(delete)) {
            return Err(Refused::Forbidden);
        }
        if let Some(kept) = inner.kept.remove(handle) {
            inner.bytes = inner.bytes.saturating_sub(kept.sealed.len());
        }
        Ok(())
    }
}

/// [`super::handle`]'s answer for the shelf at `handle`.
pub(super) async fn handle(
    web: &Web,
    request: Request<hyper::body::Incoming>,
    handle: &str,
) -> Response<Body> {
    let now_ns = tilo_clock::unix_now_ns();
    match *request.method() {
        Method::OPTIONS => reply(StatusCode::NO_CONTENT, Vec::new()),
        Method::GET => match web.shelf.get(handle, now_ns) {
            Some(sealed) => json_status(StatusCode::OK, &Fetched { sealed }),
            None => gone(),
        },
        Method::POST => {
            let (_, bytes) = match body(request, MAX_SEALED_BYTES + 1024).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
                return status(StatusCode::BAD_REQUEST, "not a sealed invitation");
            };
            match web.shelf.put(handle, stored, now_ns) {
                Ok(()) => reply(StatusCode::NO_CONTENT, Vec::new()),
                Err(refusal) => refused(&refusal),
            }
        }
        Method::DELETE => {
            let (_, bytes) = match body(request, 1024).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            let Ok(take_back) = serde_json::from_slice::<TakeBack>(&bytes) else {
                return status(StatusCode::BAD_REQUEST, "not a take-back");
            };
            match web.shelf.take(handle, &take_back.delete, now_ns) {
                Ok(()) => reply(StatusCode::NO_CONTENT, Vec::new()),
                Err(refusal) => refused(&refusal),
            }
        }
        _ => status(StatusCode::METHOD_NOT_ALLOWED, "GET, POST or DELETE"),
    }
}

/// What the shelf answers when it holds nothing under a handle: whether it
/// lapsed or was taken back is the inviter's business, not the reader's.
fn gone() -> Response<Body> {
    status(
        StatusCode::GONE,
        "this invitation is gone: it lapsed, or its inviter took it back",
    )
}

fn refused(refusal: &Refused) -> Response<Body> {
    match refusal {
        Refused::Malformed(why) => status(StatusCode::BAD_REQUEST, why),
        Refused::Taken => status(StatusCode::CONFLICT, "that handle is taken"),
        Refused::Full => status(StatusCode::SERVICE_UNAVAILABLE, "the shelf is full"),
        Refused::Gone => gone(),
        Refused::Forbidden => status(StatusCode::FORBIDDEN, "that is not its delete secret"),
    }
}

/// CORS for the shelf: any site's page may leave and fetch an invitation, and
/// no credentials ride along.
pub(super) fn cors(response: &mut Response<Body>) {
    use hyper::header;
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("content-type"),
    );
}

/// Whether `text` is [`DELETE_CHARS`] characters of base64url.
fn spelled(text: &str) -> bool {
    text.chars().count() == DELETE_CHARS
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn digest(secret: &str) -> [u8; 32] {
    *blake3::hash(secret.as_bytes()).as_bytes()
}

/// Whether two digests are equal, comparing all of both whatever they hold.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;

    const NOW: u64 = 1_700_000_000 * NS;
    const HANDLE: &str = "abcdefghijklmnop";
    const DELETE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn stored(sealed: &str, expires_in_s: u64) -> Stored {
        Stored {
            sealed: sealed.to_owned(),
            expires_at_ns: NOW.saturating_add(expires_in_s.saturating_mul(NS)),
            delete: DELETE.to_owned(),
        }
    }

    #[test]
    fn what_is_kept_is_handed_back_until_it_lapses() {
        let shelf = Shelf::default();
        shelf
            .put(HANDLE, stored(&"a".repeat(1200), 60), NOW)
            .unwrap();
        assert_eq!(shelf.get(HANDLE, NOW), Some("a".repeat(1200)));
        assert_eq!(shelf.get(HANDLE, NOW + 59 * NS), Some("a".repeat(1200)));
        assert_eq!(
            shelf.get(HANDLE, NOW + 60 * NS),
            None,
            "nothing past its life"
        );
    }

    #[test]
    fn an_invitation_is_taken_back_only_with_its_own_secret() {
        let shelf = Shelf::default();
        shelf.put(HANDLE, stored("c2VhbGVk", 60), NOW).unwrap();
        let wrong = "b".repeat(DELETE_CHARS);
        assert!(matches!(
            shelf.take(HANDLE, &wrong, NOW),
            Err(Refused::Forbidden)
        ));
        assert!(
            shelf.get(HANDLE, NOW).is_some(),
            "a wrong secret changes nothing"
        );
        shelf.take(HANDLE, DELETE, NOW).unwrap();
        assert!(shelf.get(HANDLE, NOW).is_none());
        assert!(matches!(
            shelf.take(HANDLE, DELETE, NOW),
            Err(Refused::Gone)
        ));
    }

    #[test]
    fn a_handle_is_kept_once_and_only_for_a_while_worth_keeping() {
        let shelf = Shelf::default();
        shelf.put(HANDLE, stored("c2VhbGVk", 60), NOW).unwrap();
        assert!(matches!(
            shelf.put(HANDLE, stored("b3RoZXI", 60), NOW),
            Err(Refused::Taken)
        ));
        for refused in [
            Stored {
                sealed: String::new(),
                ..stored("c2VhbGVk", 60)
            },
            Stored {
                sealed: "a".repeat(MAX_SEALED_BYTES + 1),
                ..stored("c2VhbGVk", 60)
            },
            Stored {
                delete: "short".to_owned(),
                ..stored("c2VhbGVk", 60)
            },
            Stored {
                expires_at_ns: NOW,
                ..stored("c2VhbGVk", 60)
            },
            Stored {
                expires_at_ns: NOW + (MAX_STORED_SECONDS + 1) * NS,
                ..stored("c2VhbGVk", 60)
            },
        ] {
            assert!(matches!(
                shelf.put("ponmlkjihgfedcba", refused, NOW),
                Err(Refused::Malformed(_))
            ));
        }
        assert!(shelf.get("ponmlkjihgfedcba", NOW).is_none());
    }

    #[test]
    fn the_shelf_holds_only_so_much() {
        let shelf = Shelf::default();
        for i in 0..MAX_ENTRIES {
            let handle = format!("{i:016}");
            shelf
                .put(&handle, stored("c2VhbGVk", 60), NOW)
                .unwrap_or_else(|_| panic!("entry {i}"));
        }
        assert!(matches!(
            shelf.put("zzzzzzzzzzzzzzzz", stored("c2VhbGVk", 60), NOW),
            Err(Refused::Full)
        ));
        // What lapses no longer takes room.
        let later = NOW + 60 * NS;
        shelf
            .put(
                "zzzzzzzzzzzzzzzz",
                Stored {
                    expires_at_ns: later + 60 * NS,
                    ..stored("c2VhbGVk", 0)
                },
                later,
            )
            .unwrap();
        assert_eq!(shelf.inner().kept.len(), 1);
    }
}
