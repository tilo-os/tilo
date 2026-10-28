//! `WebDAV` and Nextcloud accounts as a [`cloud_source::CloudSource`].
//!
//! One provider covers both, because Nextcloud is a `WebDAV` server with
//! extensions. What the Nextcloud flavour adds:
//!
//! - folder `getetag`s that change when anything below them changes, so a
//!   poll skips unchanged subtrees ([`Capabilities::recursive_folder_revisions`]);
//! - `oc:checksums` (SHA-1/MD5) as provider hashes and `oc:fileid` for the
//!   versions endpoint;
//! - chunked upload sessions (`uploads/<user>/<transfer>`), resumable after
//!   a restart;
//! - file versions (list, read, restore) and a trash bin.
//!
//! Any server that answers an RFC 6578 `sync-collection` REPORT on the root
//! with `sync-level` infinity gets a cursor change feed ([`DavSource::probe`]);
//! otherwise the daemon polls.
//!
//! Transport policy follows Files' former browser: redirects are refused (a
//! request body is never replayed and credentials never follow a hop), every
//! response href must stay on the account's origin and under its root, and
//! listings are size-bounded. Errors never carry URLs or bodies.

#![allow(
    clippy::missing_errors_doc,
    reason = "fallible methods return cloud_source::Error, documented there"
)]

use std::ops::Range;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use cloud_source::{
    Capabilities, Change, ChangeFeed, ChangePage, CloudSource, ContentHash, Credential,
    CredentialSource, Cursor, Download, Error, HashAlgorithm, Item, ItemId, ItemKind, Quota,
    Result, Revision, RevisionInfo, UploadSession, UploadState, UploadTarget, http,
};
use tilo_dav::{
    Credentials, DavClient, DavResponse, Depth, Limits, Precondition, PropName, StatusCode, Url,
    multistatus, props,
};

pub mod account;
#[cfg(feature = "fake-server")]
pub mod fake;
pub mod paths;
mod upload;

pub use account::{AccountError, config_for_account};

/// Largest `PROPFIND`/REPORT body read for one listing.
pub const MAX_LISTING_BYTES: usize = 64 * 1024 * 1024;
/// Default Nextcloud upload chunk.
pub const DEFAULT_UPLOAD_CHUNK: u64 = 10 * 1024 * 1024;

const OWNCLOUD: &str = "http://owncloud.org/ns";
const OC_FILEID: PropName = PropName {
    namespace: OWNCLOUD,
    name: "fileid",
};
const OC_CHECKSUMS: PropName = PropName {
    namespace: OWNCLOUD,
    name: "checksums",
};
const QUOTA_USED: PropName = PropName::dav("quota-used-bytes");
const QUOTA_AVAILABLE: PropName = PropName::dav("quota-available-bytes");

const ITEM_PROPS: [PropName; 5] = [
    props::GETCONTENTLENGTH,
    props::GETCONTENTTYPE,
    props::GETETAG,
    props::GETLASTMODIFIED,
    props::RESOURCETYPE,
];
const NEXTCLOUD_PROPS: [PropName; 7] = [
    props::GETCONTENTLENGTH,
    props::GETCONTENTTYPE,
    props::GETETAG,
    props::GETLASTMODIFIED,
    props::RESOURCETYPE,
    OC_FILEID,
    OC_CHECKSUMS,
];

/// `true` when `tag` is `W/` followed by the opaque tag of `revision`.
fn is_weak_form_of(tag: &str, revision: &Revision) -> bool {
    tag.strip_prefix("W/") == Some(revision.as_str())
}

fn listing_limits() -> Limits {
    Limits {
        max_bytes: MAX_LISTING_BYTES,
        max_responses: 1_000_000,
        max_depth: 32,
        max_nodes: 20_000_000,
        max_href_bytes: 8 * 1024,
    }
}

/// Which server dialect an account speaks.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Flavor {
    /// A plain RFC 4918 server.
    Generic,
    /// Nextcloud (or `ownCloud`): `base` is the server URL, `user` the login
    /// the `files/<user>` root, uploads and versions live under.
    Nextcloud { base: Url, user: String },
}

/// Where and how to reach one account. Contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DavConfig {
    /// The collection URL of the account root, ending in `/`.
    pub root: Url,
    pub flavor: Flavor,
    /// Bytes per chunk of a Nextcloud upload session; larger files use a
    /// session, smaller ones a single `PUT`.
    pub upload_chunk: u64,
}

impl DavConfig {
    pub fn generic(root: Url) -> Self {
        Self {
            root: with_slash(root),
            flavor: Flavor::Generic,
            upload_chunk: DEFAULT_UPLOAD_CHUNK,
        }
    }

    pub fn nextcloud(base: Url, user: &str) -> Option<Self> {
        let mut root = with_slash(base.clone());
        root.path_segments_mut().ok()?.pop_if_empty().extend([
            "remote.php",
            "dav",
            "files",
            user,
            "",
        ]);
        Some(Self {
            root,
            flavor: Flavor::Nextcloud {
                base: with_slash(base),
                user: user.to_owned(),
            },
            upload_chunk: DEFAULT_UPLOAD_CHUNK,
        })
    }

    fn is_nextcloud(&self) -> bool {
        matches!(self.flavor, Flavor::Nextcloud { .. })
    }

    /// `<base>/remote.php/dav/<area>/<user>/<rest…>`.
    fn nextcloud_url(&self, area: &str, rest: &[&str], folder: bool) -> Option<Url> {
        let Flavor::Nextcloud { base, user } = &self.flavor else {
            return None;
        };
        let mut url = base.clone();
        {
            let mut segments = url.path_segments_mut().ok()?;
            segments
                .pop_if_empty()
                .extend(["remote.php", "dav", area, user.as_str()]);
            segments.extend(rest);
            if folder {
                segments.push("");
            }
        }
        Some(url)
    }
}

fn with_slash(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

/// A mounted `WebDAV`/Nextcloud account. Cloning shares the connection
/// pool, the credential source and the probed feed.
#[derive(Clone)]
pub struct DavSource {
    config: DavConfig,
    client: DavClient,
    credentials: Arc<dyn CredentialSource>,
    /// Whether the root answers `sync-collection` (decided by [`Self::probe`]).
    sync_collection: Arc<RwLock<bool>>,
}

impl DavSource {
    /// A source for `config` authenticating with `credentials`. No request
    /// is made until the first call.
    pub fn new(config: DavConfig, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let client = DavClient::new(&tilo_dav::Options {
            user_agent: Some("Tilo-Storage/0.1"),
            // Whole-request deadline: large ranged reads and chunk uploads
            // stream for a while; the connect timeout catches dead servers.
            timeout: Duration::from_secs(30 * 60),
            connect_timeout: Some(Duration::from_secs(10)),
            redirects: tilo_dav::Redirects::Refuse,
        })
        // The HTTP client could not be built at all; it never reached a
        // server, which is the same failure mode as a refused connection.
        .map_err(|error| match error {
            tilo_dav::Error::Build(error) => transport_error(tilo_dav::Error::Build(error)),
            // `DavClient::new` only ever answers with `Build`.
            _ => Error::InvalidResponse("client build"),
        })?;
        Ok(Self {
            config,
            client,
            credentials,
            sync_collection: Arc::new(RwLock::new(false)),
        })
    }

    pub fn config(&self) -> &DavConfig {
        &self.config
    }

    /// Ask the server whether it offers a `sync-collection` change feed on
    /// the root and switch to it if so. Returns whether it does; a server
    /// that answers, and answers that it cannot (a refused status, a body
    /// that is not a `sync-collection` report), does not offer one. Only a
    /// request that got no answer at all is an error, so the caller can tell
    /// an unreachable server from an unsupported one.
    pub fn probe(&self) -> Result<bool> {
        let supported = match self.sync_report("") {
            Ok(_) => true,
            Err(error) if error.is_transport_failure() => return Err(error),
            Err(_) => false,
        };
        if let Ok(mut flag) = self.sync_collection.write() {
            *flag = supported;
        }
        Ok(supported)
    }

    fn uses_sync_collection(&self) -> bool {
        self.sync_collection.read().is_ok_and(|flag| *flag)
    }

    fn client(&self) -> Result<DavClient> {
        let credentials = match self.credentials.credential()? {
            Credential::Basic { username, password } => {
                Credentials::new(username, password.as_str())
            }
            Credential::Bearer { token, .. } => Credentials::bearer(token.as_str()),
            Credential::Session { .. } => return Err(Error::Unsupported("session credentials")),
        };
        Ok(self.client.with_credentials(credentials))
    }

    fn url(&self, id: &ItemId, folder: bool) -> Result<Url> {
        paths::url_for(&self.config.root, id, folder).ok_or(Error::NotFound)
    }

    fn props(&self) -> &'static [PropName] {
        if self.config.is_nextcloud() {
            &NEXTCLOUD_PROPS
        } else {
            &ITEM_PROPS
        }
    }

    /// The response of a request that is expected whatever it answers, so a
    /// failure to send can say why before the caller reads the status
    /// (`upload`'s calls, which match on the status themselves).
    fn answered(sent: std::result::Result<DavResponse, tilo_dav::Error>) -> Result<DavResponse> {
        sent.map_err(transport_error)
    }

    /// Map a transport result and the response status to our errors.
    fn check(&self, sent: Result<DavResponse>) -> Result<DavResponse> {
        let response = sent?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(self.status_error(&response))
    }

    /// A response that has to be a `multistatus`: anything else is the
    /// status the server answered with, which says more than "no body".
    fn demanded(
        &self,
        sent: Result<DavResponse>,
        status: StatusCode,
        what: &'static str,
    ) -> Result<tilo_dav::Multistatus> {
        let response = self.check(sent)?;
        if response.status() != status {
            return Err(self.status_error(&response));
        }
        Self::multistatus(response, what)
    }

    fn status_error(&self, response: &DavResponse) -> Error {
        // The status is the cause a person and a conformance case read; the
        // retry meaning of a given status is still the caller's, through
        // `Error::is_transient`.
        let status = response.status().as_u16();
        let retry_after = response
            .header("retry-after")
            .and_then(|value| http::parse_retry_after(&value, SystemTime::now()));
        match status {
            401 => http::unauthorized(self.credentials.as_ref()),
            status => http::classify(status, retry_after).unwrap_or(Error::Status(status)),
        }
    }

    fn propfind(&self, url: Url, depth: Depth) -> Result<tilo_dav::Multistatus> {
        let sent = self.client()?.propfind(url, depth, self.props());
        let response = self.check(sent.map_err(transport_error))?;
        Self::listing(response)
    }

    /// `PROPFIND` of an item whose kind is not known yet, at its bare path.
    /// A server that names collections with their trailing slash (Apache
    /// `mod_dav` behind `DirectorySlash`) answers a folder's bare path with
    /// a redirect to the slashed one; that is the same item, so it is asked
    /// there. Any other redirect stays refused.
    fn propfind_item(&self, id: &ItemId, depth: Depth) -> Result<tilo_dav::Multistatus> {
        let bare = self.url(id, false)?;
        let sent = self.client()?.propfind(bare.clone(), depth, self.props());
        let folder = self.url(id, true)?;
        let to_folder = sent.as_ref().is_ok_and(|response| {
            response.status().is_redirection()
                && response
                    .header("location")
                    .and_then(|location| bare.join(&location).ok())
                    .is_some_and(|target| target == folder)
        });
        let response = if to_folder {
            self.check(
                self.client()?
                    .propfind(folder, depth, self.props())
                    .map_err(transport_error),
            )?
        } else {
            self.check(sent.map_err(transport_error))?
        };
        Self::listing(response)
    }

    fn listing(response: DavResponse) -> Result<tilo_dav::Multistatus> {
        Self::multistatus(response, "multistatus")
    }

    /// The `multistatus` document a response declares, read under the
    /// listing limits.
    fn multistatus(response: DavResponse, what: &'static str) -> Result<tilo_dav::Multistatus> {
        let bytes = response
            .read_bounded(MAX_LISTING_BYTES)
            .map_err(describe_failure)?;
        Self::describe(multistatus::parse_bytes(&bytes, &listing_limits()), what)
    }

    /// A listing body that could not be parsed keeps its cause: which way
    /// the document was unusable (a non-`multistatus` root, malformed XML,
    /// a rejected href, a node or depth cap), or that no body arrived to
    /// parse at all.
    fn describe(
        listing: std::result::Result<tilo_dav::Multistatus, tilo_dav::XmlError>,
        what: &'static str,
    ) -> Result<tilo_dav::Multistatus> {
        listing.map_err(|error| Error::InvalidResponse(xml_failure(&error, what)))
    }

    fn item_from(&self, response: &tilo_dav::Response) -> Option<Item> {
        let id = paths::id_for_href(&self.config.root, &response.href)?;
        let folder = response.is_collection();
        let parents = if id.as_str().is_empty() {
            Vec::new()
        } else {
            vec![paths::parent_of(&id)]
        };
        let name = paths::name_of(&id).to_owned();
        let hashes = response
            .prop_text("checksums")
            .map(parse_checksums)
            .unwrap_or_default();
        Some(Item {
            parents,
            name,
            kind: if folder {
                ItemKind::Folder
            } else {
                ItemKind::File
            },
            size: if folder {
                None
            } else {
                response
                    .prop_text("getcontentlength")
                    .and_then(|v| v.parse().ok())
            },
            modified: response
                .prop_text("getlastmodified")
                .and_then(paths::parse_http_date),
            revision: response.prop_text("getetag").map(Revision::new),
            hashes,
            mime: response.prop_text("getcontenttype").map(ToOwned::to_owned),
            id,
        })
    }

    fn fileid(&self, id: &ItemId) -> Result<String> {
        let listing = self.propfind_item(id, Depth::Zero)?;
        listing
            .responses
            .first()
            .and_then(|r| r.prop_text("fileid"))
            .map(ToOwned::to_owned)
            .ok_or(Error::Unsupported("file ids"))
    }

    /// The sync REPORT a feed starts with, read the way [`DavSource::probe`]
    /// asks it: a server that answers something other than a `multistatus`
    /// has no feed, rather than failing.
    fn sync_report(&self, token: &str) -> Result<tilo_dav::Multistatus> {
        let response = self.check(self.report(token))?;
        if response.status() != StatusCode::MULTI_STATUS {
            return Err(Error::Unsupported("change feed"));
        }
        Self::multistatus(response, "sync-collection")
    }

    /// RFC 6578 §3.2: an invalid token is a 403 (or 409 on some servers)
    /// with `valid-sync-token`.
    fn forgot_token(response: &DavResponse) -> bool {
        matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::CONFLICT
        )
    }

    fn report(&self, token: &str) -> Result<DavResponse> {
        let sent = self.client()?.report(
            self.config.root.clone(),
            Depth::Zero,
            sync_body(token, self.props()),
        );
        sent.map_err(transport_error)
    }

    fn precondition(expect: Option<&Revision>) -> Precondition<'_> {
        expect.map_or(Precondition::None, |r| Precondition::IfMatch(r.as_str()))
    }

    fn download(
        &self,
        url: Url,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        let sent = self
            .client()?
            .get_range(url.clone(), range.clone(), Self::precondition(expect));
        let response = match (sent, expect) {
            (Ok(refused), Some(expected))
                if refused.status() == StatusCode::PRECONDITION_FAILED
                    && refused
                        .etag()
                        .is_some_and(|tag| is_weak_form_of(&tag, expected)) =>
            {
                self.read_freshly_written(url, range.clone(), expected)?
            }
            (sent, _) => self.check(sent.map_err(transport_error))?,
        };
        let revision = response.etag().map(Revision::new);
        let served = http::Served {
            partial: response.status() == StatusCode::PARTIAL_CONTENT,
            content_range: response
                .header("content-range")
                .as_deref()
                .and_then(http::parse_content_range),
            declared: response.content_length(),
        };
        let download = http::ranged_body(response.into_reader(), range, served)?;
        Ok(Download {
            revision,
            ..download
        })
    }

    /// A read of `expected` that the server refused only because it still
    /// marks that version's entity tag weak: Apache does so for a file
    /// modified within the last second, and `If-Match` compares strongly.
    /// The same bytes are read unconditionally and accepted only when the
    /// server names the same version.
    fn read_freshly_written(
        &self,
        url: Url,
        range: Option<Range<u64>>,
        expected: &Revision,
    ) -> Result<DavResponse> {
        let sent = self.client()?.get_range(url, range, Precondition::None);
        let response = self.check(sent.map_err(transport_error))?;
        if response
            .etag()
            .is_some_and(|tag| tag == expected.as_str() || is_weak_form_of(&tag, expected))
        {
            Ok(response)
        } else {
            Err(Error::Conflict { current: None })
        }
    }

    /// `true` when `id` exists.
    fn exists(&self, id: &ItemId) -> Result<bool> {
        match self.stat(id) {
            Ok(_) => Ok(true),
            Err(Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// The errors that say the request never got an answer: an unreachable
/// server, a reset or timeout, a body that stopped part-way, or a client that
/// could not be built. Distinguishing these from a server that answered
/// wrongly is what [`DavSource::probe`] needs to call a server "not
/// supporting" a feed rather than failing.
trait TransportFailure {
    fn is_transport_failure(&self) -> bool;
}

impl TransportFailure for Error {
    fn is_transport_failure(&self) -> bool {
        matches!(
            self,
            Self::Offline | Self::InvalidResponse("transport" | "client build" | "body cut short")
        )
    }
}

/// A request that never got an answer. Retrying is the same decision it was
/// as [`Error::Offline`] (`Error::is_transient` covers it), but the failure
/// says which: a transport error (a reset, a refused connection, a failed
/// TLS handshake, a request that ran out of time) or a client that could not
/// be built at all.
fn transport_error(error: tilo_dav::Error) -> Error {
    match error {
        // `Offline` names a server that is not there, which is what a
        // connection that never got made looks like; every other transport
        // failure says what came between the request and the answer.
        error @ tilo_dav::Error::Transport(_) if unreached(&error) => Error::Offline,
        tilo_dav::Error::Transport(_) => Error::InvalidResponse("transport"),
        tilo_dav::Error::Build(_) => Error::InvalidResponse("client build"),
        // A send only yields the two arms above; these arrive from a path
        // that also reads, and keep their own cause there.
        tilo_dav::Error::Read(error) => Error::InvalidResponse(read_failure(&error)),
        tilo_dav::Error::Xml(error) => Error::InvalidResponse(xml_failure(&error, "multistatus")),
        tilo_dav::Error::TooLarge => Error::TooLarge,
    }
}

/// A body that never arrived in full keeps its cause: the transport that
/// cut it, the size cap, or a client that could not be built.
fn describe_failure(error: tilo_dav::Error) -> Error {
    match error {
        tilo_dav::Error::TooLarge => Error::TooLarge,
        tilo_dav::Error::Read(error) => Error::InvalidResponse(read_failure(&error)),
        // A send only fails as `Build` or `Transport`; the other two arrive
        // from a path that parses in the same step.
        tilo_dav::Error::Xml(error) => Error::InvalidResponse(xml_failure(&error, "multistatus")),
        error @ (tilo_dav::Error::Build(_) | tilo_dav::Error::Transport(_)) => {
            transport_error(error)
        }
    }
}

/// `true` when the request never reached a server at all: the connection was
/// refused, the host is gone, a handshake failed, or a proxy is not there. A
/// connection that opened and then broke — a reset, a timeout — is a
/// transport failure of its own, so callers can tell an unreachable server
/// from one that stopped answering mid-request.
fn unreached(error: &tilo_dav::Error) -> bool {
    let tilo_dav::Error::Transport(error) = error else {
        return false;
    };
    if error.is_connect() || error.is_timeout() {
        return true;
    }
    // A connect error that reqwest did not classify still names its reason
    // where the source chain carries an OS error.
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::NetworkUnreachable
            );
        }
        cause = current.source();
    }
    false
}

/// Why a body could not be read to its end.
fn read_failure(error: &std::io::Error) -> &'static str {
    // reqwest reports a body that stopped under its own kind, with the
    // reason (an `UnexpectedEof` from a declared length that was not sent, a
    // reset) further down the source chain.
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            return read_failure(io);
        }
        cause = current.source();
    }
    match error.kind() {
        std::io::ErrorKind::UnexpectedEof => "body cut short",
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::NotConnected => "connection closed during body",
        std::io::ErrorKind::TimedOut => "body timed out",
        _ => "body read",
    }
}

/// Why a listing body is not an acceptable `multistatus` document.
fn xml_failure(error: &tilo_dav::XmlError, what: &'static str) -> &'static str {
    match error {
        // Checked before decoding anything, so the size cap keeps its own
        // meaning (`Error::TooLarge`) rather than being reworded here.
        tilo_dav::XmlError::TooLarge => what,
        tilo_dav::XmlError::NotMultistatus => match what {
            "sync-collection" => "sync-collection answer is not multistatus",
            "quota" => "quota answer is not multistatus",
            _ => "answer is not multistatus",
        },
        tilo_dav::XmlError::Malformed => "answer is malformed XML",
        tilo_dav::XmlError::NotUtf8 => "answer is not UTF-8",
        tilo_dav::XmlError::Doctype => "answer carries a DTD",
        tilo_dav::XmlError::TooManyNodes => "answer has too many XML nodes",
        tilo_dav::XmlError::TooDeep => "answer nests too deeply",
        tilo_dav::XmlError::TooManyResponses => "answer lists too many resources",
        tilo_dav::XmlError::InvalidHref => "answer carries an invalid href",
    }
}

/// Parse `oc:checksums` (`SHA1:… MD5:…`).
fn parse_checksums(text: &str) -> Vec<ContentHash> {
    text.split_whitespace()
        .filter_map(|entry| {
            let (algorithm, value) = entry.split_once(':')?;
            let algorithm = match algorithm.to_ascii_uppercase().as_str() {
                "MD5" => HashAlgorithm::Md5,
                "SHA1" => HashAlgorithm::Sha1,
                "SHA256" => HashAlgorithm::Sha256,
                other => HashAlgorithm::Other(other.to_owned()),
            };
            Some(ContentHash {
                algorithm,
                value: value.to_ascii_lowercase(),
            })
        })
        .collect()
}

fn sync_body(token: &str, props: &[PropName]) -> String {
    // `tilo_dav::body::sync_collection_body` asks for sync-level 1; a file
    // tree needs every depth.
    tilo_dav::body::sync_collection_body(token, props).replace(
        "<D:sync-level>1</D:sync-level>",
        "<D:sync-level>infinite</D:sync-level>",
    )
}

impl CloudSource for DavSource {
    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::webdav();
        if self.uses_sync_collection() {
            caps.change_feed = ChangeFeed::Cursor;
        }
        if self.config.is_nextcloud() {
            caps.recursive_folder_revisions = true;
            caps.revisions = true;
            caps.hashes = vec![HashAlgorithm::Sha1, HashAlgorithm::Md5];
            caps.trash = true;
            caps.quota = true;
            caps.resumable_upload = true;
            caps.upload_chunk = self.config.upload_chunk.max(1);
        }
        caps
    }

    fn root(&self) -> ItemId {
        ItemId::new("")
    }

    fn stat(&self, id: &ItemId) -> Result<Item> {
        let listing = self.propfind_item(id, Depth::Zero)?;
        listing
            .responses
            .iter()
            .filter(|r| r.has_success())
            .find_map(|r| self.item_from(r).filter(|item| &item.id == id))
            .ok_or(Error::NotFound)
    }

    fn list(&self, folder: &ItemId) -> Result<Vec<Item>> {
        let listing = self.propfind(self.url(folder, true)?, Depth::One)?;
        Ok(listing
            .responses
            .iter()
            .filter(|r| r.has_success())
            .filter_map(|r| self.item_from(r))
            .filter(|item| &item.id != folder && item.parents.first() == Some(folder))
            .collect())
    }

    fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        if !self.uses_sync_collection() {
            return Err(Error::Unsupported("change feed"));
        }
        let token = cursor.map_or("", |c| c.0.as_str());
        let sent = self.report(token);
        // A cursor the server forgot restarts the enumeration, whether the
        // feed is read here or probed at start-up.
        if !token.is_empty() && sent.as_ref().is_ok_and(Self::forgot_token) {
            return Err(Error::CursorExpired);
        }
        let listing = self.demanded(sent, StatusCode::MULTI_STATUS, "sync-collection")?;
        let next = listing
            .sync_token
            .clone()
            .ok_or(Error::InvalidResponse("sync-token"))?;
        let mut changes = Vec::new();
        for response in &listing.responses {
            let Some(id) = paths::id_for_href(&self.config.root, &response.href) else {
                continue;
            };
            if id.as_str().is_empty() {
                continue;
            }
            if response.status == Some(404) {
                changes.push(Change::Remove(id));
            } else if let Some(item) = self.item_from(response) {
                changes.push(Change::Upsert(item));
            }
        }
        Ok(ChangePage {
            changes,
            cursor: Cursor(next),
            has_more: false,
            reset: token.is_empty(),
        })
    }

    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        self.download(self.url(id, false)?, range, expect)
    }

    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>> {
        let (id, precondition) = match &target {
            UploadTarget::Replace { id, expect } => {
                (id.clone(), upload::Condition::Match(expect.clone()))
            }
            UploadTarget::Create { parent, name } => (
                paths::child_of(parent, name).ok_or(Error::InvalidResponse("file name"))?,
                upload::Condition::Create,
            ),
        };
        // Nextcloud can keep the `ETag` across an overwrite; the session
        // needs the revision it replaces to make sure it ends at a new one.
        let replaced = match &precondition {
            _ if !self.config.is_nextcloud() => None,
            upload::Condition::Match(Some(revision)) => Some(revision.clone()),
            upload::Condition::Match(None) => match self.stat(&id) {
                Ok(item) => item.revision,
                Err(Error::NotFound) => None,
                Err(error) => return Err(error),
            },
            upload::Condition::Create => None,
        };
        if self.config.is_nextcloud() && size > self.config.upload_chunk {
            return upload::Chunked::begin(self, id, precondition, replaced, size)
                .map(|s| Box::new(s) as Box<dyn UploadSession>);
        }
        Ok(Box::new(upload::SinglePut::new(
            self,
            id,
            precondition,
            replaced,
            size,
        )))
    }

    fn resume_upload(&self, state: &UploadState) -> Result<Box<dyn UploadSession>> {
        upload::Chunked::resume(self, state).map(|s| Box::new(s) as Box<dyn UploadSession>)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item> {
        let id = paths::child_of(parent, name).ok_or(Error::InvalidResponse("folder name"))?;
        let response = Self::answered(self.client()?.mkcol(self.url(&id, true)?))?;
        match response.status() {
            status if status.is_success() => self.stat(&id),
            StatusCode::METHOD_NOT_ALLOWED => Err(Error::AlreadyExists),
            StatusCode::CONFLICT => Err(Error::NotFound),
            _ => Err(self.status_error(&response)),
        }
    }

    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item> {
        let current = self.stat(id)?;
        let folder = current.is_folder();
        let destination_id =
            paths::child_of(new_parent, new_name).ok_or(Error::InvalidResponse("file name"))?;
        if &destination_id == id {
            return Ok(current);
        }
        let destination = self.url(&destination_id, folder)?;
        let response = Self::answered(self.client()?.move_to(
            self.url(id, folder)?,
            &destination,
            false,
            Self::precondition(expect),
        ))?;
        match response.status() {
            status if status.is_success() => self.stat(&destination_id),
            // `Overwrite: F` onto an existing item and a failed `If-Match`
            // are both 412; tell them apart.
            StatusCode::PRECONDITION_FAILED => {
                if self.exists(&destination_id)? {
                    Err(Error::AlreadyExists)
                } else {
                    Err(Error::Conflict {
                        current: current.revision,
                    })
                }
            }
            StatusCode::CONFLICT => Err(Error::NotFound),
            _ => Err(self.status_error(&response)),
        }
    }

    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()> {
        let folder = self.stat(id)?.is_folder();
        let response = Self::answered(
            self.client()?
                .delete(self.url(id, folder)?, Self::precondition(expect)),
        )?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.status_error(&response))
        }
    }

    fn revisions(&self, id: &ItemId) -> Result<Vec<RevisionInfo>> {
        let fileid = self.fileid(id)?;
        let url = self
            .config
            .nextcloud_url("versions", &["versions", &fileid], true)
            .ok_or(Error::Unsupported("revisions"))?;
        let listing = self.propfind(url.clone(), Depth::One)?;
        let mut out: Vec<RevisionInfo> = listing
            .responses
            .iter()
            .filter(|r| r.has_success() && !r.is_collection())
            .filter_map(|r| {
                let version = paths::id_for_href(&url, &r.href)?;
                (!version.as_str().is_empty()).then(|| RevisionInfo {
                    revision: Revision::new(version.0),
                    modified: r
                        .prop_text("getlastmodified")
                        .and_then(paths::parse_http_date),
                    size: r.prop_text("getcontentlength").and_then(|v| v.parse().ok()),
                    author: None,
                })
            })
            .collect();
        out.sort_by_key(|info| std::cmp::Reverse(info.modified));
        Ok(out)
    }

    fn read_revision(
        &self,
        id: &ItemId,
        revision: &Revision,
        range: Option<Range<u64>>,
    ) -> Result<Download> {
        let fileid = self.fileid(id)?;
        let url = self
            .config
            .nextcloud_url("versions", &["versions", &fileid, revision.as_str()], false)
            .ok_or(Error::Unsupported("revisions"))?;
        self.download(url, range, None)
    }

    fn restore_revision(&self, id: &ItemId, revision: &Revision) -> Result<Item> {
        let fileid = self.fileid(id)?;
        let source = self
            .config
            .nextcloud_url("versions", &["versions", &fileid, revision.as_str()], false)
            .ok_or(Error::Unsupported("revisions"))?;
        let target = self
            .config
            .nextcloud_url("versions", &["restore", "target"], false)
            .ok_or(Error::Unsupported("revisions"))?;
        let sent = self
            .client()?
            .move_to(source, &target, true, Precondition::None);
        self.check(sent.map_err(transport_error))?;
        self.stat(id)
    }

    fn quota(&self) -> Result<Quota> {
        let sent = self.client()?.propfind(
            self.config.root.clone(),
            Depth::Zero,
            &[QUOTA_USED, QUOTA_AVAILABLE],
        );
        let response = self.check(sent.map_err(transport_error))?;
        let listing = Self::multistatus(response, "quota")?;
        let first = listing
            .responses
            .first()
            .ok_or(Error::Unsupported("quota"))?;
        let used: u64 = first
            .prop_text("quota-used-bytes")
            .and_then(|v| v.parse().ok())
            .ok_or(Error::Unsupported("quota"))?;
        // Negative "available" values mean unknown/unlimited.
        let available: Option<u64> = first
            .prop_text("quota-available-bytes")
            .and_then(|v| v.parse().ok());
        Ok(Quota {
            used,
            total: available.map(|a| a.saturating_add(used)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextcloud_roots_and_endpoints_derive_from_the_server_url() {
        let config =
            DavConfig::nextcloud(Url::parse("https://cloud.example/base").unwrap(), "ada").unwrap();
        assert_eq!(
            config.root.as_str(),
            "https://cloud.example/base/remote.php/dav/files/ada/"
        );
        assert_eq!(
            config
                .nextcloud_url("uploads", &["t1", "00001"], false)
                .unwrap()
                .as_str(),
            "https://cloud.example/base/remote.php/dav/uploads/ada/t1/00001"
        );
    }

    #[test]
    fn checksums_parse_into_provider_hashes() {
        let hashes = parse_checksums("SHA1:ABC MD5:def ADLER32:1");
        assert_eq!(hashes.len(), 3);
        assert_eq!(hashes[0].algorithm, HashAlgorithm::Sha1);
        assert_eq!(hashes[0].value, "abc");
    }
}
