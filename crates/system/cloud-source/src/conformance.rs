//! The provider conformance suite.
//!
//! One suite, three layers, no changes to the checks between them:
//!
//! 1. **In-process fakes**, on every PR: the provider's fake server, with
//!    fault injection ([`Fault`]).
//! 2. **Real self-hostable servers** in containers in CI (Nextcloud, a plain
//!    `WebDAV` server).
//! 3. **Live provider accounts** on a schedule, with credentials from CI
//!    secrets; skipped when the secrets are absent.
//!
//! What makes that possible is that a [`Fixture`] only needs things every
//! layer can provide: the source under test, a second independent client to
//! the same account ("another device", used to change things behind the
//! source's back), and a [`SwitchableCredential`] in front of the real
//! credential. Going offline and injecting faults are optional; checks that
//! need them skip when the fixture returns `false`.
//!
//! **Isolation.** The source's [`CloudSource::root`] must be an empty folder
//! dedicated to this fixture (a fresh `conformance-<uuid>` folder in a live
//! account), and the fixture deletes it when dropped. Checks create only
//! what they need under it, so runs never see each other.
//!
//! A provider crate enables the `conformance` feature from its
//! dev-dependencies, implements [`Fixture`] and expands
//! [`conformance_suite!`](crate::conformance_suite) once per layer:
//!
//! ```ignore
//! cloud_source::conformance_suite!(fake, FakeFixture::start());
//! cloud_source::conformance_suite!(live, match LiveFixture::from_env() {
//!     Some(fixture) => fixture,
//!     None => return, // secrets absent: skipped
//! });
//! ```
//!
//! Checks that depend on an optional capability consult
//! [`CloudSource::capabilities`] and pass trivially when it is not declared,
//! so a provider's declared capabilities are exactly what is verified.
//!
//! The checks panic on failure: they are test bodies.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "conformance checks are test bodies; a panic is the failure report"
)]

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use zeroize::Zeroizing;

use crate::{
    Change, ChangeFeed, CloudSource, Credential, CredentialSource, Cursor, Error, Item, ItemId,
    ItemKind, Notify, Revision, UploadTarget,
};

/// A fault a fake server can inject into its next response(s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The next request is answered 429 with `Retry-After: 1`.
    RateLimited,
    /// The next request is answered 503.
    ServerError,
    /// The next body is cut off before its declared length.
    TruncatedBody,
    /// The next download body is cut off by closing the connection, with
    /// no `Content-Length` declared: only the client's own length check can
    /// tell.
    ClosedEarly,
    /// The next request is answered 401 as if the token had expired; the
    /// credential works again afterwards.
    ExpiredCredential,
    /// The server forgets every change cursor it issued.
    CursorReset,
}

/// A provider under test in one of the three layers.
pub trait Fixture {
    /// The source under test. Its root is an empty folder owned by this
    /// fixture, and it authenticates through [`Fixture::credentials`].
    fn source(&self) -> &dyn CloudSource;
    /// An independent client to the same account and root, standing in for
    /// another device or the provider's web UI.
    fn remote(&self) -> &dyn CloudSource;
    /// The credential source [`Fixture::source`] uses.
    fn credentials(&self) -> &SwitchableCredential;
    /// Make the server unreachable for the source (connection refused or
    /// reset), or reachable again. `false` when this layer cannot.
    fn set_offline(&self, offline: bool) -> bool {
        let _ = offline;
        false
    }
    /// Inject `fault` into the source's next request. `false` when this
    /// layer cannot (real servers and live accounts).
    fn inject(&self, fault: Fault) -> bool {
        let _ = fault;
        false
    }
    /// Create a provider-native document named `name` in the root, for
    /// providers with [`crate::Capabilities::native_documents`]. `false`
    /// when this layer cannot.
    fn put_native_remote(&self, name: &str) -> bool {
        let _ = name;
        false
    }
}

/// A [`CredentialSource`] in front of a real one that a check can switch to
/// a credential the server rejects, counting reported rejections.
pub struct SwitchableCredential {
    inner: Box<dyn CredentialSource>,
    bogus: AtomicBool,
    rejections: AtomicUsize,
}

impl SwitchableCredential {
    pub fn new(inner: Box<dyn CredentialSource>) -> Self {
        Self {
            inner,
            bogus: AtomicBool::new(false),
            rejections: AtomicUsize::new(0),
        }
    }

    /// Hand out a credential the server must reject (or the real one again).
    pub fn set_bogus(&self, bogus: bool) {
        self.bogus.store(bogus, Ordering::SeqCst);
    }

    /// How often the source reported a rejected credential.
    pub fn rejections(&self) -> usize {
        self.rejections.load(Ordering::SeqCst)
    }
}

impl CredentialSource for SwitchableCredential {
    fn credential(&self) -> crate::Result<Credential> {
        if !self.bogus.load(Ordering::SeqCst) {
            return self.inner.credential();
        }
        Ok(match self.inner.credential()? {
            Credential::Basic { username, .. } => Credential::Basic {
                username,
                password: Zeroizing::new("conformance-wrong-password".into()),
            },
            Credential::Bearer { expires, .. } => Credential::Bearer {
                token: Zeroizing::new("conformance-invalid-token".into()),
                expires,
            },
            Credential::Session { headers, expires } => Credential::Session {
                headers: headers
                    .into_iter()
                    .map(|(name, _)| (name, Zeroizing::new("conformance-invalid".into())))
                    .collect(),
                expires,
            },
        })
    }

    fn rejected(&self) {
        self.rejections.fetch_add(1, Ordering::SeqCst);
        self.inner.rejected();
    }
}

/// Expands one `#[test]` per conformance check, each on a fresh fixture
/// from `$fixture` (an expression evaluating to a fixture; it may `return`
/// to skip, e.g. when a live layer's secrets are absent).
#[macro_export]
macro_rules! conformance_suite {
    ($module:ident, $fixture:expr) => {
        mod $module {
            #[allow(
                unused_imports,
                reason = "the fixture expression may name items of the calling module or none"
            )]
            use super::*;
            $crate::conformance_suite!(@tests $fixture;
                listing_matches_stat,
                whole_and_ranged_reads,
                stale_reads_are_conflicts,
                create_refuses_to_overwrite,
                replace_with_revision,
                stale_replace_is_a_conflict,
                chunked_and_resumed_uploads,
                folders_moves_and_deletes,
                change_detection,
                change_notification,
                offline_is_reported,
                rejected_credentials_are_reported,
                revision_history,
                native_documents,
                transient_faults_are_retryable,
                truncated_bodies_are_errors,
                truncated_ranged_reads_are_errors,
                expired_credentials_recover,
                cursor_resets_restart_enumeration,
            );
        }
    };
    (@tests $fixture:expr; $($check:ident),+ $(,)?) => {
        $(
            #[test]
            #[allow(
                unreachable_code,
                clippy::diverging_sub_expression,
                reason = "a fixture expression may return early to skip a check"
            )]
            fn $check() {
                let fixture = $fixture;
                $crate::conformance::$check(&fixture);
            }
        )+
    };
}

/// Create or replace a file at `path` (relative to the root) through the
/// fixture's independent client, creating missing folders.
pub fn put_remote(fixture: &dyn Fixture, path: &str, bytes: &[u8]) {
    let remote = fixture.remote();
    let mut folder = remote.root();
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let name = parts.pop().expect("a file path");
    for part in parts {
        let existing = remote
            .list(&folder)
            .expect("list")
            .into_iter()
            .find(|item| item.name == part);
        folder = match existing {
            Some(item) => item.id,
            None => {
                remote
                    .create_folder(&folder, part)
                    .expect("create folder")
                    .id
            }
        };
    }
    let existing = remote
        .list(&folder)
        .expect("list")
        .into_iter()
        .find(|item| item.name == name);
    let target = match existing {
        Some(item) => UploadTarget::Replace {
            id: item.id,
            expect: None,
        },
        None => UploadTarget::Create {
            parent: folder,
            name: name.to_owned(),
        },
    };
    upload_bytes(remote, target, bytes);
}

/// Delete the item at `path` through the fixture's independent client.
pub fn remove_remote(fixture: &dyn Fixture, path: &str) {
    let remote = fixture.remote();
    let item = must(remote, path);
    remote.delete(&item.id, None).expect("remote delete");
}

fn read_all(source: &dyn CloudSource, id: &ItemId) -> Vec<u8> {
    let mut body = Vec::new();
    source
        .read(id, None, None)
        .expect("read")
        .body
        .read_to_end(&mut body)
        .expect("read body");
    body
}

/// The item at `path`, found by listing from the root.
pub fn lookup(source: &dyn CloudSource, path: &str) -> Option<Item> {
    let mut folder = source.root();
    let mut found = None;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let item = source
            .list(&folder)
            .expect("list")
            .into_iter()
            .find(|item| item.name == part)?;
        folder = item.id.clone();
        found = Some(item);
    }
    found
}

fn must(source: &dyn CloudSource, path: &str) -> Item {
    lookup(source, path).unwrap_or_else(|| panic!("{path} is not listed"))
}

fn upload_bytes(source: &dyn CloudSource, target: UploadTarget, bytes: &[u8]) -> Item {
    try_upload(source, target, bytes).expect("upload")
}

fn try_upload(source: &dyn CloudSource, target: UploadTarget, bytes: &[u8]) -> crate::Result<Item> {
    let owned = bytes.to_vec();
    crate::upload(source, target, bytes.len() as u64, move |offset| {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(owned.len());
        Ok(Box::new(std::io::Cursor::new(owned[start..].to_vec())))
    })
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + i / 251) % 251).unwrap())
        .collect()
}

fn bogus() -> Revision {
    Revision::new("\"conformance-stale-revision\"")
}

/// A listed item and [`CloudSource::stat`] agree; folders and files are
/// told apart; parents point at the listed folder.
pub fn listing_matches_stat(fixture: &dyn Fixture) {
    put_remote(fixture, "alpha.txt", b"alpha");
    put_remote(fixture, "Docs/beta.txt", b"beta!");
    let source = fixture.source();
    let root = source.list(&source.root()).expect("list root");
    let alpha = root.iter().find(|i| i.name == "alpha.txt").expect("alpha");
    let docs = root.iter().find(|i| i.name == "Docs").expect("Docs");
    assert!(alpha.is_file());
    assert!(docs.is_folder());
    assert_eq!(alpha.size, Some(5));
    assert!(alpha.revision.is_some(), "files carry a revision");
    assert!(alpha.parents.contains(&source.root()));
    let stat = source.stat(&alpha.id).expect("stat");
    assert_eq!(stat.id, alpha.id);
    assert_eq!(stat.size, alpha.size);
    assert_eq!(stat.revision, alpha.revision);
    let inner = source.list(&docs.id).expect("list Docs");
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].name, "beta.txt");
    assert!(inner[0].parents.contains(&docs.id));
    assert!(matches!(
        source.stat(&ItemId::new("conformance-missing-item")),
        Err(Error::NotFound)
    ));
}

/// Whole reads and ranged reads return exactly the requested bytes.
pub fn whole_and_ranged_reads(fixture: &dyn Fixture) {
    let bytes = pattern(100_000);
    put_remote(fixture, "big.bin", &bytes);
    let source = fixture.source();
    let item = must(source, "big.bin");
    assert_eq!(read_all(source, &item.id), bytes);
    for range in [1_000..61_000_u64, 0..1, 99_999..100_000] {
        let mut download = source
            .read(&item.id, Some(range.clone()), item.revision.as_ref())
            .expect("ranged read");
        assert_eq!(download.range, range);
        let mut got = Vec::new();
        download.body.read_to_end(&mut got).expect("range body");
        let expected =
            &bytes[usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()];
        assert_eq!(got, expected, "range {range:?}");
    }
}

/// A read pinned to a revision the file no longer has fails instead of
/// returning bytes of another version.
pub fn stale_reads_are_conflicts(fixture: &dyn Fixture) {
    put_remote(fixture, "pinned.txt", b"one");
    let source = fixture.source();
    let item = must(source, "pinned.txt");
    put_remote(fixture, "pinned.txt", b"two, longer");
    let result = source.read(&item.id, Some(0..3), item.revision.as_ref());
    assert!(
        matches!(result, Err(Error::Conflict { .. })),
        "reading an outdated revision must conflict: {result:?}"
    );
}

/// Creating a file never replaces one that exists.
pub fn create_refuses_to_overwrite(fixture: &dyn Fixture) {
    let source = fixture.source();
    let created = upload_bytes(
        source,
        UploadTarget::Create {
            parent: source.root(),
            name: "new.txt".into(),
        },
        b"fresh",
    );
    assert_eq!(created.name, "new.txt");
    assert_eq!(created.size, Some(5));
    assert!(created.revision.is_some());
    assert_eq!(read_all(source, &created.id), b"fresh");
    if !source.capabilities().duplicate_names {
        let again = try_upload(
            source,
            UploadTarget::Create {
                parent: source.root(),
                name: "new.txt".into(),
            },
            b"clobber",
        );
        assert!(matches!(again, Err(Error::AlreadyExists)), "{again:?}");
        assert_eq!(read_all(source, &created.id), b"fresh");
    }
}

/// Replacing with the current revision succeeds and yields a new one.
pub fn replace_with_revision(fixture: &dyn Fixture) {
    put_remote(fixture, "doc.txt", b"v1");
    let source = fixture.source();
    let item = must(source, "doc.txt");
    let replaced = try_upload(
        source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"version two",
    )
    .unwrap_or_else(|error| {
        let before_read = source.stat(&item.id);
        let bytes = read_all(source, &item.id);
        let after_read = source.stat(&item.id);
        panic!(
            "replace failed: {error:?}; original={item:?}; before_read={before_read:?}; bytes={bytes:?}; after_read={after_read:?}"
        );
    });
    assert_eq!(replaced.id, item.id, "a replace keeps the item's identity");
    assert_ne!(replaced.revision, item.revision);
    assert_eq!(replaced.size, Some(11));
    assert_eq!(read_all(source, &item.id), b"version two");
}

/// Replacing with an outdated revision is a conflict and leaves the remote
/// bytes alone.
pub fn stale_replace_is_a_conflict(fixture: &dyn Fixture) {
    if !fixture.source().capabilities().conditional_writes {
        return;
    }
    put_remote(fixture, "shared.txt", b"base");
    let source = fixture.source();
    let item = must(source, "shared.txt");
    put_remote(fixture, "shared.txt", b"theirs");
    let result = try_upload(
        source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"mine",
    );
    assert!(matches!(result, Err(Error::Conflict { .. })), "{result:?}");
    assert_eq!(read_all(source, &item.id), b"theirs");
}

/// Uploads larger than one chunk arrive intact; resumable sessions continue
/// from their persisted state.
pub fn chunked_and_resumed_uploads(fixture: &dyn Fixture) {
    const MAX_CHUNK: u64 = 16 << 20;
    let source = fixture.source();
    let caps = source.capabilities();
    // Without sessions an upload is one request; keep it modest.
    let chunk = if caps.resumable_upload {
        caps.upload_chunk.min(MAX_CHUNK)
    } else {
        1 << 20
    };
    let bytes = pattern(usize::try_from(chunk).unwrap() * 2 + 17);
    let item = upload_bytes(
        source,
        UploadTarget::Create {
            parent: source.root(),
            name: "chunked.bin".into(),
        },
        &bytes,
    );
    assert_eq!(item.size, Some(bytes.len() as u64));
    assert_eq!(read_all(source, &item.id), bytes);

    if !caps.resumable_upload || caps.upload_chunk > MAX_CHUNK {
        return;
    }
    let len = bytes.len() as u64;
    let mut session = source
        .begin_upload(
            UploadTarget::Replace {
                id: item.id.clone(),
                expect: item.revision.clone(),
            },
            len,
        )
        .expect("begin");
    let first = caps.upload_chunk;
    let head = bytes[..usize::try_from(first).unwrap()].to_vec();
    session
        .write_chunk(Box::new(std::io::Cursor::new(head)), first)
        .expect("first chunk");
    let state = session
        .state()
        .expect("a declared resumable upload larger than one chunk has a state");
    drop(session);
    let mut resumed = source.resume_upload(&state).expect("resume");
    assert_eq!(resumed.offset(), first);
    let owned = bytes.clone();
    crate::drive(resumed.as_mut(), len, caps.upload_chunk, &mut |offset| {
        let start = usize::try_from(offset).unwrap();
        Ok(Box::new(std::io::Cursor::new(owned[start..].to_vec())))
    })
    .expect("remaining chunks");
    resumed.finish().expect("finish");
    assert_eq!(read_all(source, &item.id), bytes);
}

/// Folder creation, rename/move with preconditions, and delete.
pub fn folders_moves_and_deletes(fixture: &dyn Fixture) {
    put_remote(fixture, "move-me.txt", b"payload");
    put_remote(fixture, "taken.txt", b"occupied");
    let source = fixture.source();
    let root = source.root();
    let folder = source.create_folder(&root, "Archive").expect("mkdir");
    assert!(folder.is_folder());
    assert_eq!(folder.name, "Archive");
    if !source.capabilities().duplicate_names {
        assert!(matches!(
            source.create_folder(&root, "Archive"),
            Err(Error::AlreadyExists)
        ));
    }
    let item = must(source, "move-me.txt");
    let conditional = source.capabilities().conditional_writes;
    if conditional {
        assert!(matches!(
            source.move_item(&item.id, &folder.id, "moved.txt", Some(&bogus())),
            Err(Error::Conflict { .. })
        ));
    }
    if !source.capabilities().duplicate_names {
        assert!(matches!(
            source.move_item(&item.id, &root, "taken.txt", item.revision.as_ref()),
            Err(Error::AlreadyExists)
        ));
        assert_eq!(read_all(source, &must(source, "taken.txt").id), b"occupied");
    }
    let moved = source
        .move_item(&item.id, &folder.id, "moved.txt", item.revision.as_ref())
        .expect("move");
    assert_eq!(moved.name, "moved.txt");
    assert!(moved.parents.contains(&folder.id));
    assert!(lookup(source, "move-me.txt").is_none());
    let listed = must(source, "Archive/moved.txt");
    assert_eq!(read_all(source, &listed.id), b"payload");

    if conditional {
        assert!(matches!(
            source.delete(&listed.id, Some(&bogus())),
            Err(Error::Conflict { .. })
        ));
    }
    source
        .delete(&listed.id, listed.revision.as_ref())
        .expect("delete");
    assert!(lookup(source, "Archive/moved.txt").is_none());
    assert!(matches!(source.stat(&listed.id), Err(Error::NotFound)));
    source.delete(&folder.id, None).expect("delete folder");
    assert!(lookup(source, "Archive").is_none());
}

fn drain(source: &dyn CloudSource, cursor: Option<&Cursor>) -> (Vec<Change>, Cursor, bool) {
    let mut changes = Vec::new();
    let mut page = source.changes(cursor).expect("changes");
    let reset = page.reset;
    changes.append(&mut page.changes);
    let mut guard = 0_u32;
    while page.has_more {
        guard += 1;
        assert!(guard < 10_000, "the change feed never ends");
        page = source.changes(Some(&page.cursor)).expect("next page");
        changes.append(&mut page.changes);
    }
    (changes, page.cursor, reset)
}

/// Remote changes are observable: through the cursor feed where declared,
/// else through revisions (recursive folder revisions where declared).
pub fn change_detection(fixture: &dyn Fixture) {
    put_remote(fixture, "keep.txt", b"k");
    put_remote(fixture, "Docs/edit.txt", b"e1");
    put_remote(fixture, "Docs/gone.txt", b"g");
    let source = fixture.source();
    let caps = source.capabilities();
    match caps.change_feed {
        ChangeFeed::Cursor => {
            let (initial, cursor, reset) = drain(source, None);
            assert!(reset, "the first enumeration is a reset");
            let names: Vec<&str> = initial
                .iter()
                .filter_map(|c| match c {
                    Change::Upsert(item) => Some(item.name.as_str()),
                    Change::Remove(_) => None,
                })
                .collect();
            for expected in ["keep.txt", "Docs", "edit.txt", "gone.txt"] {
                assert!(
                    names.contains(&expected),
                    "{expected} missing from {names:?}"
                );
            }
            let edit = must(source, "Docs/edit.txt");
            let gone = must(source, "Docs/gone.txt");
            put_remote(fixture, "Docs/edit.txt", b"e2, changed");
            put_remote(fixture, "Docs/added.txt", b"a");
            remove_remote(fixture, "Docs/gone.txt");
            let (delta, _, reset) = drain(source, Some(&cursor));
            if reset {
                // A provider may restart the enumeration; then absence means
                // removal.
                assert!(
                    !delta
                        .iter()
                        .any(|c| matches!(c, Change::Upsert(item) if item.id == gone.id))
                );
            } else {
                assert!(
                    delta
                        .iter()
                        .any(|c| matches!(c, Change::Remove(id) if *id == gone.id))
                );
            }
            assert!(delta.iter().any(|c| matches!(
                c,
                Change::Upsert(item) if item.id == edit.id && item.revision != edit.revision
            )));
            assert!(
                delta
                    .iter()
                    .any(|c| matches!(c, Change::Upsert(item) if item.name == "added.txt"))
            );
        }
        ChangeFeed::Poll => {
            let docs = must(source, "Docs");
            let edit = must(source, "Docs/edit.txt");
            put_remote(fixture, "Docs/edit.txt", b"e2, changed");
            let edited = must(source, "Docs/edit.txt");
            assert_ne!(
                edited.revision, edit.revision,
                "a modified file changes revision"
            );
            if caps.recursive_folder_revisions {
                let after = source.stat(&docs.id).expect("stat Docs");
                assert_ne!(
                    after.revision, docs.revision,
                    "a declared recursive folder revision must change with its contents"
                );
            }
        }
    }
}

/// Where the provider declares notification, a change made after the
/// cursor was taken wakes [`CloudSource::wait`].
pub fn change_notification(fixture: &dyn Fixture) {
    let source = fixture.source();
    let caps = source.capabilities();
    if caps.notify == Notify::None {
        return;
    }
    let cursor = match caps.change_feed {
        ChangeFeed::Cursor => Some(drain(source, None).1),
        ChangeFeed::Poll => None,
    };
    put_remote(fixture, "wake.txt", b"w");
    let outcome = source
        .wait(cursor.as_ref(), Duration::from_secs(5))
        .expect("wait");
    assert_eq!(outcome, crate::Wait::Changed);
}

/// An unreachable server is [`Error::Offline`], and the source recovers.
pub fn offline_is_reported(fixture: &dyn Fixture) {
    put_remote(fixture, "here.txt", b"x");
    let source = fixture.source();
    if !fixture.set_offline(true) {
        return;
    }
    let result = source.list(&source.root());
    fixture.set_offline(false);
    assert!(matches!(result, Err(Error::Offline)), "{result:?}");
    assert!(result.unwrap_err().is_transient());
    assert!(lookup(source, "here.txt").is_some());
}

/// A rejected credential is [`Error::Unauthorized`] and is reported to the
/// credential source so Settings can refresh or ask the person to sign in.
pub fn rejected_credentials_are_reported(fixture: &dyn Fixture) {
    let source = fixture.source();
    let credentials = fixture.credentials();
    let before = credentials.rejections();
    credentials.set_bogus(true);
    let result = source.list(&source.root());
    credentials.set_bogus(false);
    assert!(matches!(result, Err(Error::Unauthorized)), "{result:?}");
    assert!(credentials.rejections() > before);
    source.list(&source.root()).expect("list after sign-in");
}

/// Declared revision history lists, reads and restores older versions.
pub fn revision_history(fixture: &dyn Fixture) {
    let source = fixture.source();
    if !source.capabilities().revisions {
        return;
    }
    put_remote(fixture, "history.txt", b"first");
    let item = must(source, "history.txt");
    let current = upload_bytes(
        source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"second version",
    );
    let history = source.revisions(&item.id).expect("revisions");
    let old = history
        .iter()
        .find(|info| Some(&info.revision) != current.revision.as_ref() && info.size == Some(5))
        .unwrap_or_else(|| panic!("the first version is listed: {history:?}"));
    let mut body = Vec::new();
    source
        .read_revision(&item.id, &old.revision, None)
        .expect("read revision")
        .body
        .read_to_end(&mut body)
        .expect("revision body");
    assert_eq!(body, b"first");
    source
        .restore_revision(&item.id, &old.revision)
        .expect("restore");
    assert_eq!(read_all(source, &item.id), b"first");
}

/// Provider-native documents are listed as such, have no readable body and
/// export on demand.
pub fn native_documents(fixture: &dyn Fixture) {
    let source = fixture.source();
    if !source.capabilities().native_documents {
        return;
    }
    if !fixture.put_native_remote("Plan") {
        return;
    }
    let item = must(source, "Plan");
    let ItemKind::Native(document) = &item.kind else {
        panic!("a native document is not a file: {item:?}");
    };
    assert!(!document.link_extension.is_empty());
    assert!(source.read(&item.id, None, None).is_err());
    if let Some(format) = document.exports.first() {
        let mut body = Vec::new();
        source
            .export(&item.id, format)
            .expect("export")
            .body
            .read_to_end(&mut body)
            .expect("export body");
        assert!(!body.is_empty());
    }
}

/// Rate limits and server errors are transient errors, and the next
/// request succeeds.
pub fn transient_faults_are_retryable(fixture: &dyn Fixture) {
    put_remote(fixture, "steady.txt", b"s");
    let source = fixture.source();
    for fault in [Fault::RateLimited, Fault::ServerError] {
        if !fixture.inject(fault) {
            return;
        }
        let result = source.list(&source.root());
        let error = result.expect_err("the injected fault surfaces");
        assert!(
            error.is_transient(),
            "{fault:?} must be transient: {error:?}"
        );
        if fault == Fault::RateLimited {
            assert!(matches!(error, Error::RateLimited { .. }), "{error:?}");
        }
        assert!(lookup(source, "steady.txt").is_some());
    }
}

/// A body cut short is an error, never a silently short file.
pub fn truncated_bodies_are_errors(fixture: &dyn Fixture) {
    let bytes = pattern(50_000);
    put_remote(fixture, "cut.bin", &bytes);
    let source = fixture.source();
    let item = must(source, "cut.bin");
    if !fixture.inject(Fault::TruncatedBody) {
        return;
    }
    let outcome = source.read(&item.id, None, None).and_then(|mut download| {
        let mut body = Vec::new();
        download.body.read_to_end(&mut body)?;
        Ok(body)
    });
    match outcome {
        Err(_) => {}
        Ok(body) => panic!(
            "a truncated body was accepted ({} of {} bytes)",
            body.len(),
            bytes.len()
        ),
    }
    assert_eq!(read_all(source, &item.id), bytes);
}

/// A ranged body cut short by a closed connection is an error even though
/// no length was declared: the requested range is the length to check.
pub fn truncated_ranged_reads_are_errors(fixture: &dyn Fixture) {
    let bytes = pattern(50_000);
    put_remote(fixture, "cut-range.bin", &bytes);
    let source = fixture.source();
    let item = must(source, "cut-range.bin");
    if !fixture.inject(Fault::ClosedEarly) {
        return;
    }
    let range = 1_000..41_000_u64;
    let outcome = source
        .read(&item.id, Some(range.clone()), None)
        .and_then(|mut download| {
            let mut body = Vec::new();
            download.body.read_to_end(&mut body)?;
            Ok(body)
        });
    if let Ok(body) = outcome {
        panic!(
            "a ranged body cut short was accepted ({} of {} bytes)",
            body.len(),
            range.end - range.start
        );
    }
    let mut body = Vec::new();
    source
        .read(&item.id, Some(range), None)
        .expect("ranged read")
        .body
        .read_to_end(&mut body)
        .expect("range body");
    assert_eq!(body, &bytes[1_000..41_000]);
}

/// An expired credential is reported once and the refreshed one works.
pub fn expired_credentials_recover(fixture: &dyn Fixture) {
    let source = fixture.source();
    let before = fixture.credentials().rejections();
    if !fixture.inject(Fault::ExpiredCredential) {
        return;
    }
    let result = source.list(&source.root());
    assert!(matches!(result, Err(Error::Unauthorized)), "{result:?}");
    assert!(fixture.credentials().rejections() > before);
    source
        .list(&source.root())
        .expect("the refreshed credential works");
}

/// A forgotten cursor restarts the enumeration instead of losing changes.
pub fn cursor_resets_restart_enumeration(fixture: &dyn Fixture) {
    put_remote(fixture, "before.txt", b"b");
    let source = fixture.source();
    if source.capabilities().change_feed != ChangeFeed::Cursor {
        return;
    }
    let (_, cursor, _) = drain(source, None);
    if !fixture.inject(Fault::CursorReset) {
        return;
    }
    put_remote(fixture, "after.txt", b"a");
    match source.changes(Some(&cursor)) {
        Err(Error::CursorExpired) => {
            let (all, _, reset) = drain(source, None);
            assert!(reset);
            assert!(
                all.iter()
                    .any(|c| matches!(c, Change::Upsert(item) if item.name == "after.txt"))
            );
        }
        Ok(page) => assert!(
            page.reset,
            "a forgotten cursor must restart the enumeration"
        ),
        Err(other) => panic!("unexpected error for a reset cursor: {other:?}"),
    }
}
