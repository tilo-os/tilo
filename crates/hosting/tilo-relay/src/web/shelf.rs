//! The shelf a site keeps sealed invitations on: `/i/<handle>`.
//!
//! An invitation's link carries a handle and, in its fragment, the key that
//! opens what the handle names. The bytes themselves wait here: ciphertext a
//! site has no key for, until the invitation it holds lapses or the inviter
//! takes it back. Neither the key nor the invitation is ever written down;
//! the delete secret is kept as a digest, so what a site holds cannot be used
//! to read an invitation even by the site itself.
//!
//! What a site keeps waits on its state disk, one file per handle beside its
//! other state, because a link is pasted into a message and read days later:
//! an invitation survives the relay being restarted, replaced or deployed
//! over, and is taken off the disk when it lapses. Each file is written
//! whole, by a rename into place, so a crash mid-write leaves the invitation
//! it replaced or the one it wrote, never half of either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
/// Characters a handle is spelled in, as the link's path carries it.
const HANDLE_CHARS: usize = 16;
/// Nanoseconds in a second, for the window an invitation may be kept for.
const NS: u64 = 1_000_000_000;

/// What a site keeps in place of an invitation.
struct Kept {
    /// The sealed signed invitation, base64url.
    sealed: String,
    /// The origin the invitation admits into, as the inviter named it.
    host: Option<String>,
    /// When the invitation it holds lapses.
    expires_at_ns: u64,
    /// The digest of the secret that takes it back.
    delete: [u8; 32],
}

/// What a site keeps, as the file under `dir` holds it: what a restart reads.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Written {
    /// The sealed signed invitation, base64url.
    sealed: String,
    /// The origin the invitation admits into, when the inviter named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    /// When the invitation it holds lapses, in Unix nanoseconds.
    expires_at_ns: u64,
    /// The digest of the secret that takes it back, hex. No site holds the
    /// secret itself: a file read by anyone else still takes nothing back.
    delete: String,
}

impl Written {
    fn of(kept: &Kept) -> Self {
        Self {
            sealed: kept.sealed.clone(),
            host: kept.host.clone(),
            expires_at_ns: kept.expires_at_ns,
            delete: hex::encode(kept.delete),
        }
    }

    /// What was written down, unless it is not one the site wrote.
    fn kept(self) -> Option<Kept> {
        let digest: [u8; 32] = hex::decode(&self.delete).ok()?.try_into().ok()?;
        Some(Kept {
            sealed: self.sealed,
            host: self.host,
            expires_at_ns: self.expires_at_ns,
            delete: digest,
        })
    }
}

#[derive(Default)]
struct Inner {
    kept: HashMap<String, Kept>,
    bytes: usize,
}

impl Inner {
    /// Read what `dir` holds from before this start: the invitations a link
    /// pasted into a message may still name. What lapsed while nothing was
    /// running is collected here, as it is on every other pass.
    fn opening(&mut self, dir: &Path, now_ns: u64) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(%error, dir = %dir.display(), "the shelf's invitations were not read");
                return;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(handle) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|handle| handle_spelled(handle))
            else {
                // A rename's own file, left by a crash mid-write: it names no
                // invitation, and the one it would have replaced is untouched.
                continue;
            };
            let Some(kept) = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Written>(&bytes).ok())
                .and_then(Written::kept)
            else {
                tracing::warn!(handle, "a file in the shelf is not an invitation");
                continue;
            };
            if kept.expires_at_ns <= now_ns {
                collecting(dir, handle);
                continue;
            }
            if self.kept.len() >= MAX_ENTRIES
                || self.bytes.saturating_add(kept.sealed.len()) > MAX_BYTES
            {
                tracing::warn!(
                    handle,
                    "the shelf holds as much as it will: an invitation is left for a later start"
                );
                continue;
            }
            self.bytes = self.bytes.saturating_add(kept.sealed.len());
            self.kept.insert(handle.to_owned(), kept);
        }
    }

    /// Drop what has lapsed: a site keeps nothing past an invitation's life,
    /// on its disk as in its memory.
    fn sweep(&mut self, dir: &Path, now_ns: u64) {
        let lapsed: Vec<String> = self
            .kept
            .iter()
            .filter(|(_, kept)| kept.expires_at_ns <= now_ns)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in lapsed {
            if let Some(kept) = self.kept.remove(&handle) {
                self.bytes = self.bytes.saturating_sub(kept.sealed.len());
                collecting(dir, &handle);
            }
        }
    }
}

/// Take what `handle` named off the disk. A file that is already gone is what
/// was asked for; one that will not go is said, and left for the next pass:
/// the invitation it holds is out of memory either way, so nothing answers
/// with it — a restart reads it, finds it lapsed, and tries again.
fn collecting(dir: &Path, handle: &str) {
    match std::fs::remove_file(dir.join(format!("{handle}.json"))) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, handle, "an invitation that lapsed could not be taken off the shelf");
        }
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
    /// The shelf's disk could not be written or read.
    Storage,
}

/// The invitations a site keeps, and nothing it can read.
pub(super) struct Shelf {
    /// Where what is kept waits across a restart.
    dir: PathBuf,
    inner: Mutex<Inner>,
}

impl Shelf {
    /// The shelf kept in `dir`, with what an earlier start left there.
    pub(super) fn opening(dir: PathBuf, now_ns: u64) -> Self {
        let mut inner = Inner::default();
        inner.opening(&dir, now_ns);
        Self {
            dir,
            inner: Mutex::new(inner),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Where an invitation is written down: one file per handle, named by it.
    fn path(&self, handle: &str) -> PathBuf {
        self.dir.join(format!("{handle}.json"))
    }

    /// Write `kept` down, whole, before answering for it: an invitation a
    /// link names is one a restart still finds.
    fn writing(&self, handle: &str, kept: &Kept) -> Result<(), Refused> {
        std::fs::create_dir_all(&self.dir).map_err(|error| {
            tracing::warn!(%error, handle, "the shelf's dir could not be made");
            Refused::Storage
        })?;
        let bytes = serde_json::to_vec(&Written::of(kept)).map_err(|error| {
            tracing::warn!(%error, handle, "an invitation could not be written down");
            Refused::Storage
        })?;
        tilo_files::native::atomic::write_with_mode(&self.path(handle), &bytes, 0o600).map_err(
            |error| {
                tracing::warn!(%error, handle, "an invitation could not be written down");
                Refused::Storage
            },
        )
    }

    /// Keep `stored` under `handle`, unless the shelf cannot hold it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    pub(super) fn put(&self, handle: &str, stored: Stored, now_ns: u64) -> Result<(), Refused> {
        if !handle_spelled(handle) {
            return Err(Refused::Malformed("a handle is 16 characters of base64url"));
        }
        if stored.sealed.is_empty() || stored.sealed.len() > MAX_SEALED_BYTES {
            return Err(Refused::Malformed("a sealed invitation is at most 8 KiB"));
        }
        if stored.host.as_deref().is_some_and(|host| !origin(host)) {
            return Err(Refused::Malformed(
                "the host a link names is an https:// origin, or an http:// one on this machine",
            ));
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
        inner.sweep(&self.dir, now_ns);
        if inner.kept.contains_key(handle) {
            return Err(Refused::Taken);
        }
        if inner.kept.len() >= MAX_ENTRIES || inner.bytes.saturating_add(bytes) > MAX_BYTES {
            return Err(Refused::Full);
        }
        let kept = Kept {
            sealed: stored.sealed,
            host: stored.host,
            expires_at_ns: stored.expires_at_ns,
            delete: digest(&stored.delete),
        };
        // The disk first: an invitation answered for is one a restart finds.
        self.writing(handle, &kept)?;
        inner.bytes = inner.bytes.saturating_add(bytes);
        inner.kept.insert(handle.to_owned(), kept);
        Ok(())
    }

    /// The sealed invitation `handle` names, while it is still there.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    fn get(&self, handle: &str, now_ns: u64) -> Option<Fetched> {
        let mut inner = self.inner();
        inner.sweep(&self.dir, now_ns);
        inner
            .kept
            .get(handle)
            .filter(|kept| kept.expires_at_ns > now_ns)
            .map(|kept| Fetched {
                sealed: kept.sealed.clone(),
                host: kept.host.clone(),
            })
    }

    /// Take an invitation back, if `delete` is the secret it was kept with.
    /// The disk lets go of it before the answer: a link its inviter took back
    /// names nothing, a restart included.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard holds the shelf for the whole of one operation"
    )]
    fn take(&self, handle: &str, delete: &str, now_ns: u64) -> Result<(), Refused> {
        let mut inner = self.inner();
        inner.sweep(&self.dir, now_ns);
        let Some(kept) = inner.kept.get(handle) else {
            return Err(Refused::Gone);
        };
        if !same(&kept.delete, &digest(delete)) {
            return Err(Refused::Forbidden);
        }
        // A file already gone (a state directory restored from before the
        // invitation was left, say) names nothing either: it is not a failure
        // to take back what is not there.
        match std::fs::remove_file(self.path(handle)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, handle, "a taken-back invitation could not be taken off the shelf");
                return Err(Refused::Storage);
            }
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
            Some(fetched) => json_status(StatusCode::OK, &fetched),
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
        Refused::Storage => status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the invitation could not be kept",
        ),
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

/// Whether `text` is an `https://` origin, or an `http://` one on this
/// machine ([`tilo_net::secure_origin`]): a scheme, a host and maybe a port,
/// and nothing else.
fn origin(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("https://").or_else(|| {
        text.strip_prefix("http://")
            .filter(|_| tilo_net::Url::parse(text).is_ok_and(|url| tilo_net::secure_origin(&url)))
    }) else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 260
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// Whether `text` is [`DELETE_CHARS`] characters of base64url.
fn spelled(text: &str) -> bool {
    text.chars().count() == DELETE_CHARS
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `text` is a handle as a link spells one ([`HANDLE_CHARS`]
/// characters of base64url): the name its invitation's file goes by, so one
/// that could name a file elsewhere is never one.
fn handle_spelled(text: &str) -> bool {
    text.chars().count() == HANDLE_CHARS
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
    const OTHER: &str = "ponmlkjihgfedcba";
    const DELETE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn stored(sealed: &str, expires_in_s: u64) -> Stored {
        Stored {
            sealed: sealed.to_owned(),
            expires_at_ns: NOW.saturating_add(expires_in_s.saturating_mul(NS)),
            delete: DELETE.to_owned(),
            host: None,
        }
    }

    fn kept(sealed: &str) -> Fetched {
        Fetched {
            sealed: sealed.to_owned(),
            host: None,
        }
    }

    /// A shelf kept in a directory of its own, as a site keeps one, and the
    /// state directory a restart would find it in.
    fn shelf_at(now_ns: u64) -> (tempfile::TempDir, Shelf) {
        let state = tempfile::tempdir().unwrap();
        let shelf = Shelf::opening(state.path().to_path_buf(), now_ns);
        (state, shelf)
    }

    #[test]
    fn what_is_kept_is_handed_back_until_it_lapses() {
        let (_state, shelf) = shelf_at(NOW);
        shelf
            .put(HANDLE, stored(&"a".repeat(1200), 60), NOW)
            .unwrap();
        assert_eq!(shelf.get(HANDLE, NOW), Some(kept(&"a".repeat(1200))));
        assert_eq!(
            shelf.get(HANDLE, NOW + 59 * NS),
            Some(kept(&"a".repeat(1200)))
        );
        assert_eq!(
            shelf.get(HANDLE, NOW + 60 * NS),
            None,
            "nothing past its life"
        );
    }

    #[test]
    fn an_invitation_left_before_a_restart_waits_for_whoever_follows_the_link() {
        let state = tempfile::tempdir().unwrap();
        let disk = state.path().to_path_buf();
        let sealed = "a".repeat(1200);
        let before = Shelf::opening(disk.clone(), NOW);
        before
            .put(
                HANDLE,
                Stored {
                    host: Some("https://relay.example".to_owned()),
                    ..stored(&sealed, 3600)
                },
                NOW,
            )
            .unwrap();
        drop(before);

        // The relay that comes up next on the same state disk finds it, as
        // whoever the link reached needs it to.
        let after = Shelf::opening(disk.clone(), NOW + 60 * NS);
        assert_eq!(
            after.get(HANDLE, NOW + 60 * NS),
            Some(Fetched {
                sealed,
                host: Some("https://relay.example".to_owned()),
            })
        );
        // The secret it was left with still takes it back, off the disk too.
        let wrong = "b".repeat(DELETE_CHARS);
        assert!(matches!(
            after.take(HANDLE, &wrong, NOW + 60 * NS),
            Err(Refused::Forbidden)
        ));
        after.take(HANDLE, DELETE, NOW + 60 * NS).unwrap();
        assert!(!disk.join(format!("{HANDLE}.json")).is_file());
        assert!(
            Shelf::opening(disk, NOW + 60 * NS)
                .get(HANDLE, NOW)
                .is_none(),
            "a link its inviter took back names nothing, a restart included"
        );
    }

    #[test]
    fn an_invitation_that_lapsed_while_nothing_ran_is_collected() {
        let (state, before) = shelf_at(NOW);
        before.put(HANDLE, stored("c2VhbGVk", 60), NOW).unwrap();
        before.put(OTHER, stored("b3RoZXI", 3600), NOW).unwrap();
        drop(before);

        // Two minutes later the relay is back: one invitation lapsed while it
        // was down, the other did not.
        let after = Shelf::opening(state.path().to_path_buf(), NOW + 120 * NS);
        assert_eq!(after.get(HANDLE, NOW + 120 * NS), None);
        assert_eq!(
            after.get(OTHER, NOW + 120 * NS),
            Some(kept("b3RoZXI")),
            "one that has not lapsed is still there"
        );
        assert!(
            !state.path().join(format!("{HANDLE}.json")).exists(),
            "what lapsed is taken off the disk"
        );
        assert!(state.path().join(format!("{OTHER}.json")).exists());
    }

    #[test]
    fn a_file_that_is_not_an_invitation_is_not_one() {
        let (state, _shelf) = shelf_at(NOW);
        // What a crash mid-write leaves: a rename's own file, which names no
        // handle, beside the invitation it would have replaced.
        std::fs::write(state.path().join(".tmpQx7f"), b"half a record").unwrap();
        // What nothing of this site wrote, under a handle it could have: no
        // invitation, and none handed out.
        std::fs::write(state.path().join(format!("{HANDLE}.json")), b"{ not one").unwrap();
        let shelf = Shelf::opening(state.path().to_path_buf(), NOW);
        assert_eq!(shelf.get(HANDLE, NOW), None);
    }

    #[test]
    fn an_invitation_is_taken_back_only_with_its_own_secret() {
        let (_state, shelf) = shelf_at(NOW);
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
        let (_state, shelf) = shelf_at(NOW);
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
                host: Some("https://host.example/not-an-origin".to_owned()),
                ..stored("c2VhbGVk", 60)
            },
            Stored {
                host: Some("http://host.example".to_owned()),
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
                shelf.put(OTHER, refused, NOW),
                Err(Refused::Malformed(_))
            ));
        }
        assert!(shelf.get(OTHER, NOW).is_none());
        // A handle that is not one names no file, wherever it points.
        for handle in ["../../../etc/passwd", "short", &format!("{HANDLE}x")] {
            assert!(matches!(
                shelf.put(handle, stored("c2VhbGVk", 60), NOW),
                Err(Refused::Malformed(_))
            ));
        }
    }

    #[test]
    fn what_it_admits_into_comes_back_with_the_invitation() {
        let (_state, shelf) = shelf_at(NOW);
        shelf
            .put(
                HANDLE,
                Stored {
                    host: Some("https://host.example:8443".to_owned()),
                    ..stored("c2VhbGVk", 60)
                },
                NOW,
            )
            .unwrap();
        assert_eq!(
            shelf.get(HANDLE, NOW),
            Some(Fetched {
                sealed: "c2VhbGVk".to_owned(),
                host: Some("https://host.example:8443".to_owned()),
            })
        );
    }

    #[test]
    fn the_shelf_holds_only_so_much() {
        let (_state, shelf) = shelf_at(NOW);
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
