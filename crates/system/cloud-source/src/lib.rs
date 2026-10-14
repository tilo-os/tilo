//! The provider interface behind mounted cloud accounts.
//!
//! A cloud account configured in Settings (`WebDAV`, Nextcloud, Google Drive,
//! `OneDrive`, Dropbox, `iCloud` Drive) becomes a folder the storage daemon
//! serves like any other part of the workspace: metadata is listed and
//! cached, file bodies hydrate on first read, local writes upload in the
//! background and conflicts keep both versions. Everything the daemon needs
//! from a provider goes through [`CloudSource`]; everything provider-specific
//! stays behind it.
//!
//! # Model
//!
//! - **Items are identified by id, projected to paths.** [`ItemId`] is the
//!   provider's stable identity (a Drive file id, a Graph item id, a Dropbox
//!   `id:`; for `WebDAV` the href relative to the account root). An [`Item`]
//!   names its parents; the daemon derives the visible tree with [`tree::Tree`],
//!   which also resolves duplicate names and items with several parents.
//! - **Change feeds are cursors.** Providers with a server-side feed (Drive
//!   `changes.list`, Graph delta, Dropbox `list_folder/continue`, the DAV
//!   `sync-collection` report) implement [`CloudSource::changes`], and
//!   [`CloudSource::wait`] where the server can hold a request open or push.
//!   Plain `WebDAV` has no feed: it declares [`ChangeFeed::Poll`] and the daemon
//!   walks [`CloudSource::list`], skipping folders whose revision did not change
//!   when [`Capabilities::recursive_folder_revisions`] holds.
//! - **Revisions are preconditions.** Every write and delete can carry the
//!   [`Revision`] (`ETag`, `rev`, version) the caller last saw; a mismatch is
//!   [`Error::Conflict`], never a silent overwrite.
//! - **Bodies stream.** [`CloudSource::read`] returns a reader over exactly the
//!   requested range; [`CloudSource::begin_upload`] returns an
//!   [`UploadSession`] fed chunk by chunk, resumable where the provider has
//!   upload sessions. Nothing here buffers a whole file.
//! - **Provider hashes detect change, BLAKE3 keys the cache.** [`ContentHash`]
//!   carries what the provider reports (MD5, SHA-1, Dropbox `content_hash`,
//!   `quickXorHash`); the daemon's blob cache keys downloaded bytes by their
//!   own BLAKE3.
//! - **Provider-native documents are not files.** Google Docs/Sheets/Slides
//!   are [`ItemKind::Native`]: read-only entries that open the web editor or
//!   export on demand.
//! - **Capabilities are declared, not guessed.** [`Capabilities`] tells the
//!   daemon and the UI which features exist so they degrade gracefully.
//! - **Credentials are borrowed, never owned.** A source asks its
//!   [`CredentialSource`] for a short-lived [`Credential`] per request and
//!   reports a rejection; Settings keeps custody, refresh and any
//!   interactive step (`OAuth` consent, a 2FA code for a session trust token).
//! - **Unofficial APIs say so.** A provider built on an undocumented API
//!   (`iCloud` Drive's web API) declares [`Support::BestEffort`], and the UI
//!   labels it.
//!
//! The [`conformance`] suite (feature `conformance`) checks any provider
//! against an in-process fake, a real server in a container, or a live
//! account, unchanged.

#![allow(
    clippy::missing_errors_doc,
    reason = "every fallible method returns the provider-neutral `Error` documented on the type"
)]

use std::fmt;
use std::io::Read;
use std::ops::Range;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[cfg(feature = "conformance")]
pub mod conformance;
pub mod http;
pub mod materialize;
pub mod tree;

/// A provider's stable identity for an item.
///
/// Opaque to everyone but the provider. For `WebDAV` it is the decoded path
/// relative to the account root (`""` is the root, `"Docs/a.txt"` a file),
/// so ids survive a server that returns absolute or relative hrefs.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl ItemId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The provider's version token for an item: an `ETag`, a Dropbox `rev`, a
/// Drive `version`. Compared for equality only; used as the precondition of
/// writes and conditional reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub String);

impl Revision {
    pub fn new(revision: impl Into<String>) -> Self {
        Self(revision.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A position in a provider's change feed. Opaque; persisted by the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(pub String);

/// Content hash algorithms providers report.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    /// Dropbox `content_hash` (SHA-256 over 4 MiB block hashes).
    DropboxContentHash,
    /// `OneDrive` `quickXorHash`.
    QuickXor,
    Blake3,
    /// Anything else, named as the provider names it.
    Other(String),
}

/// One provider-reported content hash, in the provider's own encoding
/// (lowercase hex for MD5/SHA, base64 for `quickXorHash`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash {
    pub algorithm: HashAlgorithm,
    pub value: String,
}

/// An export format of a provider-native document.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExportFormat {
    pub mime: String,
    /// Extension without the dot, e.g. `"pdf"`, `"docx"`.
    pub extension: String,
}

/// A document that only exists inside the provider (Google Docs, Sheets,
/// Slides). It has no byte body; it is opened in the provider's editor or
/// exported.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NativeDocument {
    pub mime: String,
    /// Provider-specific link suffix (`"gdoc"`, `"gsheet"`), retained for
    /// metadata consumers. The tree uses the preferred Office export suffix
    /// instead, or leaves unsupported native document names unadorned.
    pub link_extension: String,
    /// Where the provider's editor opens it.
    pub web_url: Option<String>,
    pub exports: Vec<ExportFormat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    File,
    Folder,
    Native(NativeDocument),
}

/// Metadata of one remote item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    /// Parents by id. Empty only for the root. Drive items may have several.
    pub parents: Vec<ItemId>,
    /// The provider's display name. Not unique among siblings on every
    /// provider; [`tree::Tree`] makes the projected names unique.
    pub name: String,
    pub kind: ItemKind,
    /// Byte size of a file body.
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Version token for preconditions. For a folder on a provider with
    /// [`Capabilities::recursive_folder_revisions`], it changes whenever
    /// anything below it changes.
    pub revision: Option<Revision>,
    pub hashes: Vec<ContentHash>,
    pub mime: Option<String>,
}

impl Item {
    pub fn is_folder(&self) -> bool {
        self.kind == ItemKind::Folder
    }

    pub fn is_file(&self) -> bool {
        self.kind == ItemKind::File
    }

    pub fn hash(&self, algorithm: &HashAlgorithm) -> Option<&str> {
        self.hashes
            .iter()
            .find(|hash| &hash.algorithm == algorithm)
            .map(|hash| hash.value.as_str())
    }
}

/// How the daemon learns about remote changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeFeed {
    /// [`CloudSource::changes`] returns pages from a cursor.
    Cursor,
    /// No feed: walk [`CloudSource::list`], comparing revisions.
    Poll,
}

/// Whether the provider can tell the daemon a change happened without a
/// fixed poll timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Notify {
    None,
    /// [`CloudSource::wait`] holds a request open until something changes.
    LongPoll,
    /// [`CloudSource::wait`] returns on a pushed notification.
    Push,
}

/// How far a provider's behaviour is backed by a documented API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Support {
    /// A documented, stable API.
    Official,
    /// An undocumented API that may change without notice (`iCloud` Drive's
    /// web API). Shown as such; failures are expected to need updates.
    BestEffort,
}

/// What a provider supports. The daemon and the UI consult these instead of
/// guessing from the provider's name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent feature flags, read one at a time"
)]
pub struct Capabilities {
    pub support: Support,
    pub change_feed: ChangeFeed,
    pub notify: Notify,
    /// A folder's revision changes when anything below it changes (DAV
    /// `getetag` on Nextcloud, `ctag`), so a poll can skip unchanged
    /// subtrees.
    pub recursive_folder_revisions: bool,
    /// [`CloudSource::revisions`], [`CloudSource::read_revision`] and
    /// [`CloudSource::restore_revision`] work.
    pub revisions: bool,
    /// Hashes the provider reports on items.
    pub hashes: Vec<HashAlgorithm>,
    pub share_links: bool,
    pub quota: bool,
    /// Delete moves to a provider trash instead of removing.
    pub trash: bool,
    /// [`UploadSession::state`] survives a restart.
    pub resumable_upload: bool,
    /// Preferred bytes per [`UploadSession::write_chunk`]; a single chunk
    /// for providers without upload sessions (`u64::MAX`).
    pub upload_chunk: u64,
    /// [`CloudSource::read`] honours ranges server-side. When false the
    /// provider still returns only the requested range, but pays for the
    /// skipped prefix.
    pub range_reads: bool,
    /// Writes and deletes honour a [`Revision`] precondition.
    pub conditional_writes: bool,
    pub multiple_parents: bool,
    pub duplicate_names: bool,
    /// Names differing only in case are the same item.
    pub case_insensitive: bool,
    pub native_documents: bool,
}

impl Capabilities {
    /// The baseline of a plain RFC 4918 server.
    pub fn webdav() -> Self {
        Self {
            support: Support::Official,
            change_feed: ChangeFeed::Poll,
            notify: Notify::None,
            recursive_folder_revisions: false,
            revisions: false,
            hashes: Vec::new(),
            share_links: false,
            quota: false,
            trash: false,
            resumable_upload: false,
            upload_chunk: u64::MAX,
            range_reads: true,
            conditional_writes: true,
            multiple_parents: false,
            duplicate_names: false,
            case_insensitive: false,
            native_documents: false,
        }
    }
}

/// One entry of a change page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "pages are short-lived vectors; boxing every item costs more than the padding"
)]
pub enum Change {
    /// The item is new or its metadata changed.
    Upsert(Item),
    /// The item is gone (deleted or no longer visible to this account).
    Remove(ItemId),
}

/// One page of a cursor change feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangePage {
    pub changes: Vec<Change>,
    /// Where the next call continues.
    pub cursor: Cursor,
    /// More pages are available now; call again without waiting.
    pub has_more: bool,
    /// This page begins a full enumeration (first call, or the old cursor
    /// expired). Items not upserted before `has_more` turns false are gone.
    pub reset: bool,
}

/// The outcome of [`CloudSource::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Changed,
    /// Nothing changed before the timeout; wait again.
    Idle,
    /// The server asks to wait this long before the next request.
    Backoff(Duration),
}

/// A streaming body of an item or a range of it.
pub struct Download {
    /// Exactly the bytes of `range`.
    pub body: Box<dyn Read + Send>,
    pub range: Range<u64>,
    /// Total size of the item when the server said.
    pub total: Option<u64>,
    /// The revision the bytes belong to, when the server said.
    pub revision: Option<Revision>,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("range", &self.range)
            .field("total", &self.total)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Where an upload goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadTarget {
    /// Replace an existing file's body. With `expect`, the write fails with
    /// [`Error::Conflict`] when the remote revision differs.
    Replace {
        id: ItemId,
        expect: Option<Revision>,
    },
    /// Create a new file; fails with [`Error::AlreadyExists`] if the name is
    /// taken (on providers without [`Capabilities::duplicate_names`]).
    Create { parent: ItemId, name: String },
}

/// Serialised state of a resumable upload session, persisted by the daemon
/// to continue after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UploadState(pub String);

/// An upload in progress. Chunks are written in order from
/// [`UploadSession::offset`].
pub trait UploadSession: Send {
    /// Where the next chunk starts: the bytes the server has committed.
    fn offset(&self) -> u64;
    /// State to resume from after a restart, for resumable sessions.
    fn state(&self) -> Option<UploadState>;
    /// Send the next `len` bytes, read from `data`.
    fn write_chunk(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()>;
    /// Commit the upload once every byte was written.
    fn finish(self: Box<Self>) -> Result<Item>;
    /// Discard the session and whatever it staged.
    fn abort(self: Box<Self>) -> Result<()>;
}

/// One historic version of a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionInfo {
    pub revision: Revision,
    pub modified: Option<SystemTime>,
    pub size: Option<u64>,
    pub author: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub used: u64,
    pub total: Option<u64>,
}

/// A credential for one request. Zeroized on drop.
pub enum Credential {
    Basic {
        username: String,
        password: Zeroizing<String>,
    },
    /// An `OAuth2` access token. Refresh is Settings' job; the daemon only
    /// ever sees the short-lived access token.
    Bearer {
        token: Zeroizing<String>,
        expires: Option<SystemTime>,
    },
    /// A web session: request headers (cookies, session and trust tokens)
    /// established interactively by Settings, e.g. after a 2FA step. When
    /// the server stops accepting it the source reports
    /// [`CredentialSource::rejected`] and Settings asks the person again.
    Session {
        headers: Vec<(String, Zeroizing<String>)>,
        expires: Option<SystemTime>,
    },
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .finish_non_exhaustive(),
            Self::Bearer { expires, .. } => f
                .debug_struct("Bearer")
                .field("expires", expires)
                .finish_non_exhaustive(),
            Self::Session { headers, expires } => f
                .debug_struct("Session")
                .field(
                    "headers",
                    &headers.iter().map(|(name, _)| name).collect::<Vec<_>>(),
                )
                .field("expires", expires)
                .finish_non_exhaustive(),
        }
    }
}

impl Clone for Credential {
    fn clone(&self) -> Self {
        match self {
            Self::Basic { username, password } => Self::Basic {
                username: username.clone(),
                password: password.clone(),
            },
            Self::Bearer { token, expires } => Self::Bearer {
                token: token.clone(),
                expires: *expires,
            },
            Self::Session { headers, expires } => Self::Session {
                headers: headers.clone(),
                expires: *expires,
            },
        }
    }
}

/// Hands a source the credential of its one account.
pub trait CredentialSource: Send + Sync {
    /// The current credential. [`Error::Unauthorized`] when there is none
    /// (signed out, disabled, removed).
    fn credential(&self) -> Result<Credential>;
    /// The server rejected the last credential; fetch a fresh one next time
    /// (a refreshed token) and report the account as needing sign-in if
    /// there is none.
    fn rejected(&self);
}

/// Whether credentials may be sent to `endpoint`: HTTPS, or plain HTTP to a
/// loopback host, which never leaves the machine (a local server, or a
/// test's fake). Mount configurations name their endpoints; this is the one
/// check every provider's configuration passes before a mount uses it.
pub fn is_secure_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    url.host().is_some() && (url.scheme() == "https" || (url.scheme() == "http" && loopback))
}

/// A fixed credential, for tests and servers without refresh.
pub struct StaticCredential(pub Credential);

impl CredentialSource for StaticCredential {
    fn credential(&self) -> Result<Credential> {
        Ok(self.0.clone())
    }

    fn rejected(&self) {}
}

/// Provider-neutral failures. Messages never contain credentials, URLs with
/// secrets or response bodies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server could not be reached.
    #[error("the server is unreachable")]
    Offline,
    /// The credential was missing or rejected.
    #[error("sign-in required")]
    Unauthorized,
    #[error("access denied")]
    Forbidden,
    #[error("no such item")]
    NotFound,
    /// A [`Revision`] precondition failed: someone else changed the item.
    #[error("the item changed remotely")]
    Conflict { current: Option<Revision> },
    #[error("an item with that name already exists")]
    AlreadyExists,
    /// The change cursor is no longer valid; start a full enumeration.
    #[error("the change cursor expired")]
    CursorExpired,
    /// The upload session is gone; start the upload again.
    #[error("the upload session expired")]
    SessionExpired,
    /// The server asks to slow down. One policy for every provider
    /// ([`http::classify`]): 429, 503 and 509 always, and any other 5xx
    /// that carries a `Retry-After`. `retry_after` is the server's
    /// `Retry-After` and `None` without one; no provider invents a delay,
    /// because how long to back off is the consumer's policy.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("the account is out of storage")]
    InsufficientStorage,
    #[error("the response exceeds the size limit")]
    TooLarge,
    /// The provider or this server does not offer the operation.
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    #[error("invalid server response: {0}")]
    InvalidResponse(&'static str),
    /// Any other HTTP status.
    #[error("the server answered {0}")]
    Status(u16),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Worth retrying later without user action.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Offline | Self::RateLimited { .. } | Self::Io(_) => true,
            Self::Status(status) => *status >= 500,
            _ => false,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One mounted cloud account's provider.
///
/// Blocking: the daemon calls it from worker threads. Implementations are
/// shared across threads and must be cheap to call concurrently.
pub trait CloudSource: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// The account's root folder.
    fn root(&self) -> ItemId;

    fn stat(&self, id: &ItemId) -> Result<Item>;

    /// The direct children of a folder.
    fn list(&self, folder: &ItemId) -> Result<Vec<Item>>;

    /// The next page of the change feed after `cursor`; from the beginning
    /// (a full enumeration, [`ChangePage::reset`]) when `cursor` is `None`.
    fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        let _ = cursor;
        Err(Error::Unsupported("change feed"))
    }

    /// Block until the feed has changes after `cursor`, up to about
    /// `timeout`.
    fn wait(&self, cursor: Option<&Cursor>, timeout: Duration) -> Result<Wait> {
        let _ = (cursor, timeout);
        Err(Error::Unsupported("change notification"))
    }

    /// Stream `range` of a file (the whole file when `None`). With
    /// `expect`, fails with [`Error::Conflict`] if the file is no longer at
    /// that revision, so a hydrating read never mixes versions.
    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download>;

    /// Start an upload of `size` bytes.
    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>>;

    /// Continue a session from [`UploadSession::state`].
    fn resume_upload(&self, state: &UploadState) -> Result<Box<dyn UploadSession>> {
        let _ = state;
        Err(Error::SessionExpired)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item>;

    /// Rename and/or move. Fails with [`Error::AlreadyExists`] rather than
    /// replacing an existing item.
    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item>;

    /// Delete (to the provider trash when [`Capabilities::trash`]).
    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()>;

    /// Historic versions, newest first.
    fn revisions(&self, id: &ItemId) -> Result<Vec<RevisionInfo>> {
        let _ = id;
        Err(Error::Unsupported("revisions"))
    }

    fn read_revision(
        &self,
        id: &ItemId,
        revision: &Revision,
        range: Option<Range<u64>>,
    ) -> Result<Download> {
        let _ = (id, revision, range);
        Err(Error::Unsupported("revisions"))
    }

    /// Make a historic version current again.
    fn restore_revision(&self, id: &ItemId, revision: &Revision) -> Result<Item> {
        let _ = (id, revision);
        Err(Error::Unsupported("revisions"))
    }

    /// Export a provider-native document.
    fn export(&self, id: &ItemId, format: &ExportFormat) -> Result<Download> {
        let _ = (id, format);
        Err(Error::Unsupported("export"))
    }

    fn share_link(&self, id: &ItemId) -> Result<String> {
        let _ = id;
        Err(Error::Unsupported("share links"))
    }

    fn quota(&self) -> Result<Quota> {
        Err(Error::Unsupported("quota"))
    }
}

/// Upload `size` bytes from `open(offset)` in the provider's preferred
/// chunks. `open` returns a reader positioned at `offset`, so each chunk is
/// read straight from the staged file and nothing is buffered here.
pub fn upload(
    source: &dyn CloudSource,
    target: UploadTarget,
    size: u64,
    mut open: impl FnMut(u64) -> std::io::Result<Box<dyn Read + Send>>,
) -> Result<Item> {
    let chunk = source.capabilities().upload_chunk.max(1);
    let mut session = source.begin_upload(target, size)?;
    drive(session.as_mut(), size, chunk, &mut open)?;
    session.finish()
}

/// Write the remaining chunks of `session` up to `size`.
pub fn drive(
    session: &mut dyn UploadSession,
    size: u64,
    chunk: u64,
    open: &mut dyn FnMut(u64) -> std::io::Result<Box<dyn Read + Send>>,
) -> Result<()> {
    loop {
        let offset = session.offset();
        if offset >= size {
            return Ok(());
        }
        let len = chunk.min(size.saturating_sub(offset));
        let reader = open(offset)?;
        session.write_chunk(Box::new(reader.take(len)), len)?;
        if session.offset() <= offset {
            return Err(Error::InvalidResponse("upload made no progress"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_print_their_secret() {
        let basic = Credential::Basic {
            username: "ada".into(),
            password: Zeroizing::new("hunter2".into()),
        };
        let bearer = Credential::Bearer {
            token: Zeroizing::new("ya29.secret".into()),
            expires: None,
        };
        assert!(!format!("{basic:?}").contains("hunter2"));
        assert!(!format!("{bearer:?}").contains("ya29"));
    }

    #[test]
    fn items_round_trip_through_the_metadata_cache_format() {
        let item = Item {
            id: ItemId::new("1AbC"),
            parents: vec![ItemId::new("root"), ItemId::new("shared")],
            name: "Budget".into(),
            kind: ItemKind::Native(NativeDocument {
                mime: "application/vnd.google-apps.spreadsheet".into(),
                link_extension: "gsheet".into(),
                web_url: Some("https://docs.example/1AbC".into()),
                exports: vec![ExportFormat {
                    mime: "application/pdf".into(),
                    extension: "pdf".into(),
                }],
            }),
            size: None,
            modified: Some(SystemTime::UNIX_EPOCH),
            revision: Some(Revision::new("7")),
            hashes: vec![ContentHash {
                algorithm: HashAlgorithm::Md5,
                value: "d41d8cd98f00b204e9800998ecf8427e".into(),
            }],
            mime: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(serde_json::from_str::<Item>(&json).unwrap(), item);
        assert_eq!(
            item.hash(&HashAlgorithm::Md5),
            Some("d41d8cd98f00b204e9800998ecf8427e")
        );
    }
}
