//! iCloud Drive as a [`cloud_source::CloudSource`], over the web API
//! iCloud.com itself uses.
//!
//! **Best effort.** Apple does not document or support this API; it is
//! re-implemented from rclone's `iclouddrive` backend and pyicloud, and may
//! change without notice. The source declares [`Support::BestEffort`] and
//! the UI labels it so.
//!
//! # Signing in
//!
//! [`auth`] turns an Apple ID into a [`auth::StoredSession`] (SRP sign-in,
//! the two-factor code, the trust token, `accountLogin`) and keeps it alive.
//! The session's cookies are the [`cloud_source::Credential::Session`] this
//! source borrows per request. Accounts with Advanced Data Protection are
//! refused with [`auth::AuthError::AdvancedDataProtection`]: their Drive keys
//! never reach Apple's web servers.
//!
//! # Endpoints
//!
//! - `setup.icloud.com/setup/ws/1/validate` names the account's `drivews`
//!   and `docws` partitions (looked up once per source).
//! - `drivews`: `retrieveItemDetails(InFolders)` for metadata and listings,
//!   `createFolders`, `renameItems`, `moveItems`, `moveItemsToTrash`.
//! - `docws`: `download/by_id` for a signed content URL (ranged `GET`),
//!   `upload/web` for an upload slot, then `update/documents` to commit the
//!   uploaded bytes under a name.
//!
//! Items are identified by their drivewsid (`FILE::com.apple.CloudDocs::…`),
//! which survives renames and moves within a zone. Revisions are drivews
//! `etag`s.
//!
//! # Changes
//!
//! There is no change feed and no push: [`ChangeFeed::Poll`],
//! [`Notify::None`]. The daemon polls every [`POLL_INTERVAL`], walking the
//! tree with [`ICloudDrive::list_folders`], which lists up to
//! [`LIST_BATCH`] folders per request, so a poll costs one request per tree
//! level per batch rather than one per folder, and compares item etags.
//! Folder etags are not declared recursive: Apple does not say whether a
//! folder's etag changes when something deep below it does.
//!
//! # Preconditions
//!
//! Moves, renames and trashing send the expected etag and Apple refuses a
//! stale one (`ETAG_CONFLICT`). Uploads and ranged reads check the etag with
//! a metadata read first: a race of one round trip remains.
//!
//! # Session quirks
//!
//! HTTP 401, 421 (`LOGIN_TOKEN_EXPIRED`), 450 (re-authentication required)
//! and 423 (missing PCS grant) all mean the web session is no longer
//! accepted: the credential is reported rejected and the call fails with
//! [`Error::Unauthorized`], so Settings renews it (see
//! [`auth::SessionCredentials`]). 429 and 503 with `Retry-After` are
//! [`Error::RateLimited`].

#![allow(
    clippy::missing_errors_doc,
    reason = "fallible methods return cloud_source::Error, documented there"
)]

use std::io::Read;
use std::ops::Range;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use cloud_source::{
    Capabilities, ChangeFeed, CloudSource, Credential, CredentialSource, Download, Error, Item,
    ItemId, ItemKind, Notify, Result, Revision, Support, UploadSession, UploadTarget,
};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use url::Url;
use zeroize::Zeroizing;

pub mod api;
pub mod auth;
#[cfg(feature = "fake-server")]
pub mod fake;
mod http;
pub mod time;
mod upload;

use api::{DriveItem, split_id};
use http::{Cookies, ICLOUD_ORIGIN};

/// How often the daemon should poll an iCloud Drive account. Apple throttles
/// clients that poll aggressively (503, `ACCESS_DENIED`); iCloud.com itself
/// refreshes on navigation, not on a timer.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Folders per `retrieveItemDetailsInFolders` request.
pub const LIST_BATCH: usize = 32;

/// Where a source finds the account, and which folder is its root.
/// Contains no secret: it is the non-secret mount configuration a daemon
/// persists.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ICloudConfig {
    /// `https://setup.icloud.com/setup/ws/1`.
    pub setup: Url,
    /// The folder mounted as the source's root: iCloud Drive's root by
    /// default, a subfolder for an isolated conformance run.
    pub root: ItemId,
}

impl ICloudConfig {
    /// Apple's production setup service, the whole of iCloud Drive.
    pub fn apple() -> Self {
        Self::with_endpoints(&auth::Endpoints::apple())
    }

    pub fn with_endpoints(endpoints: &auth::Endpoints) -> Self {
        Self {
            setup: endpoints.setup.clone(),
            root: ItemId::new(api::ROOT_ID),
        }
    }
}

#[derive(Clone, Debug)]
struct Services {
    drive: Url,
    docs: Url,
}

/// Cookies the servers set on responses, layered over the credential they
/// were issued for (a validate may rotate one). Dropped when Settings hands
/// out a different credential.
#[derive(Default)]
struct Overlay {
    fingerprint: [u8; 32],
    cookies: Cookies,
}

struct Inner {
    config: ICloudConfig,
    http: Client,
    credentials: Arc<dyn CredentialSource>,
    services: RwLock<Option<Services>>,
    overlay: Mutex<Overlay>,
}

/// A mounted iCloud Drive account. Cloning shares the connection pool, the
/// credential source and the discovered endpoints.
#[derive(Clone)]
pub struct ICloudDrive {
    inner: Arc<Inner>,
}

/// The session cookies for one request.
struct Session {
    cookie: Zeroizing<String>,
    fingerprint: [u8; 32],
}

enum Service {
    Drive,
    Docs,
}

/// A write's per-item status said the outcome is unknown.
const UNKNOWN_STATUS: &str = "UNKNOWN";

impl ICloudDrive {
    /// A source for `config`, authenticating with `credentials` (a
    /// [`Credential::Session`]). No request is made until the first call.
    pub fn new(config: ICloudConfig, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let http = http::client(Duration::from_secs(30 * 60)).map_err(|_| Error::Offline)?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                http,
                credentials,
                services: RwLock::new(None),
                overlay: Mutex::new(Overlay::default()),
            }),
        })
    }

    pub fn config(&self) -> &ICloudConfig {
        &self.inner.config
    }

    fn session(&self) -> Result<Session> {
        let Credential::Session { headers, .. } = self.inner.credentials.credential()? else {
            return Err(Error::Unsupported(
                "credentials other than an iCloud session",
            ));
        };
        let raw = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let fingerprint = auth::srp::hash(&[raw.as_bytes()]);
        let mut cookies = Cookies::from_header(&raw);
        if let Ok(overlay) = self.inner.overlay.lock()
            && overlay.fingerprint == fingerprint
        {
            cookies.merge(&overlay.cookies);
        }
        Ok(Session {
            cookie: cookies.header(),
            fingerprint,
        })
    }

    fn absorb(&self, session: &Session, headers: &HeaderMap) {
        if !headers.contains_key(reqwest::header::SET_COOKIE) {
            return;
        }
        if let Ok(mut overlay) = self.inner.overlay.lock() {
            if overlay.fingerprint != session.fingerprint {
                *overlay = Overlay {
                    fingerprint: session.fingerprint,
                    cookies: Cookies::default(),
                };
            }
            overlay.cookies.absorb(headers);
        }
    }

    /// Map a non-success status. Session rejections are reported to the
    /// credential source.
    fn status_error(&self, status: StatusCode, headers: &HeaderMap) -> Error {
        match status.as_u16() {
            401 | 421 | 423 | 450 => {
                self.inner.credentials.rejected();
                Error::Unauthorized
            }
            403 => Error::Forbidden,
            404 | 410 => Error::NotFound,
            412 => Error::Conflict { current: None },
            429 => Error::RateLimited {
                retry_after: http::retry_after(headers),
            },
            503 => match http::retry_after(headers) {
                Some(retry_after) => Error::RateLimited {
                    retry_after: Some(retry_after),
                },
                None => Error::Status(503),
            },
            507 => Error::InsufficientStorage,
            other => Error::Status(other),
        }
    }

    fn send(&self, session: &Session, request: RequestBuilder) -> Result<Response> {
        let response = request
            .header("Origin", ICLOUD_ORIGIN)
            .header("Referer", format!("{ICLOUD_ORIGIN}/"))
            .header("Cookie", session.cookie.as_str())
            .send()
            .map_err(|_| Error::Offline)?;
        self.absorb(session, response.headers());
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(self.status_error(response.status(), response.headers()))
        }
    }

    /// The account's drivews/docws partitions, from `validate`.
    fn services(&self) -> Result<Services> {
        if let Ok(guard) = self.inner.services.read()
            && let Some(services) = guard.as_ref()
        {
            return Ok(services.clone());
        }
        let session = self.session()?;
        let request = self
            .inner
            .http
            .post(join(&self.inner.config.setup, "validate"))
            .header("Content-Type", "application/json")
            .header("Content-Length", "0");
        let response = self.send(&session, request)?;
        let info: auth::AccountInfo = read_json(response)?;
        let cookies = Cookies::from_header(&session.cookie);
        let (drive, docs) = info.check_web_access(&cookies).map_err(Error::from)?;
        let services = Services {
            drive: Url::parse(&drive).map_err(|_| Error::InvalidResponse("drivews url"))?,
            docs: Url::parse(&docs).map_err(|_| Error::InvalidResponse("docws url"))?,
        };
        if let Ok(mut guard) = self.inner.services.write() {
            *guard = Some(services.clone());
        }
        Ok(services)
    }

    fn url(&self, service: &Service, path: &str) -> Result<Url> {
        let services = self.services()?;
        let base = match service {
            Service::Drive => services.drive,
            Service::Docs => services.docs,
        };
        Ok(join(&base, path))
    }

    fn post<T: DeserializeOwned>(&self, service: &Service, path: &str, body: &Value) -> Result<T> {
        self.post_url(self.url(service, path)?, body)
    }

    fn post_url<T: DeserializeOwned>(&self, url: Url, body: &Value) -> Result<T> {
        let session = self.session()?;
        let request = self
            .inner
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        read_json(self.send(&session, request)?)
    }

    fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        let session = self.session()?;
        read_json(self.send(&session, self.inner.http.get(url))?)
    }

    /// The details of one item, without children.
    fn details(&self, id: &ItemId) -> Result<DriveItem> {
        let body = json!([{
            "items": [{"drivewsid": id.as_str(), "partialData": false, "includeHierarchy": false}]
        }]);
        let items: Vec<DriveItem> = self.post(&Service::Drive, "retrieveItemDetails", &body)?;
        let item = items
            .into_iter()
            .find(|item| item.drivewsid == id.as_str())
            .ok_or(Error::NotFound)?;
        present(item)
    }

    /// The direct children of up to [`LIST_BATCH`] folders per request, in
    /// the order asked. Polls walk the tree with this.
    pub fn list_folders(&self, folders: &[ItemId]) -> Result<Vec<(ItemId, Vec<Item>)>> {
        let mut out = Vec::with_capacity(folders.len());
        for batch in folders.chunks(LIST_BATCH) {
            let body: Vec<Value> = batch
                .iter()
                .map(|id| json!({"drivewsid": id.as_str(), "partialData": false, "includeHierarchy": false}))
                .collect();
            let answered: Vec<DriveItem> = self.post(
                &Service::Drive,
                "retrieveItemDetailsInFolders",
                &Value::Array(body),
            )?;
            for id in batch {
                let folder = answered
                    .iter()
                    .find(|item| item.drivewsid == id.as_str())
                    .cloned()
                    .ok_or(Error::NotFound)?;
                let folder = present(folder)?;
                if !folder.is_folder() {
                    return Err(Error::NotFound);
                }
                let children = folder
                    .items
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|child| child.kind.is_some() && !child.drivewsid.is_empty())
                    .map(|child| self.item(child, Some(id)))
                    .collect();
                out.push((id.clone(), children));
            }
        }
        Ok(out)
    }

    fn item(&self, item: DriveItem, parent: Option<&ItemId>) -> Item {
        let id = ItemId::new(item.drivewsid.clone());
        let parents = if id == self.inner.config.root {
            Vec::new()
        } else {
            match (parent, item.parent_id.as_deref()) {
                (Some(parent), _) => vec![parent.clone()],
                (None, Some(parent)) if !parent.is_empty() => vec![ItemId::new(parent)],
                _ => Vec::new(),
            }
        };
        let folder = item.is_folder();
        let modified = item
            .date_modified
            .as_deref()
            .or(item.date_changed.as_deref())
            .or(item.date_created.as_deref())
            .and_then(time::parse);
        let name = item.full_name().nfc().collect();
        Item {
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
                Some(item.size.unwrap_or(0))
            },
            modified,
            revision: item.etag.map(Revision::new),
            hashes: Vec::new(),
            mime: None,
            id,
        }
    }

    /// The child of `folder` a person would call `name`, if any.
    fn find_child(&self, folder: &ItemId, name: &str) -> Result<Option<Item>> {
        let wanted = fold(name);
        Ok(self
            .list(folder)?
            .into_iter()
            .find(|child| fold(&child.name) == wanted))
    }

    /// Run a drivews write on one item and check its per-item status.
    fn write_item(&self, path: &str, body: &Value) -> Result<Option<DriveItem>> {
        let answer: api::Items = self.post(&Service::Drive, path, body)?;
        let item = answer
            .items
            .into_iter()
            .next()
            .ok_or(Error::InvalidResponse("empty drive answer"))?;
        if is_unknown(&item) {
            return Ok(None);
        }
        status_result(&item)?;
        Ok(Some(item))
    }

    fn trash(&self, id: &ItemId, etag: &str) -> Result<()> {
        let body =
            json!({"items": [{"drivewsid": id.as_str(), "etag": etag, "clientId": id.as_str()}]});
        match self.write_item("moveItemsToTrash", &body)? {
            Some(_) => Ok(()),
            // Apple could not say; the item is gone if it cannot be found.
            None => match self.stat(id) {
                Err(Error::NotFound) => Ok(()),
                Ok(_) => Err(Error::InvalidResponse(
                    "the trash request did not take effect",
                )),
                Err(error) => Err(error),
            },
        }
    }

    fn rename(&self, id: &ItemId, etag: &str, name: &str) -> Result<()> {
        let body = json!({"items": [{"drivewsid": id.as_str(), "name": name, "etag": etag}]});
        self.write_item("renameItems", &body).map(|_| ())
    }

    fn download_url(&self, id: &ItemId) -> Result<Url> {
        let (_, zone, doc) = split_id(id.as_str());
        let mut url = self.url(&Service::Docs, &format!("ws/{zone}/download/by_id"))?;
        url.query_pairs_mut().append_pair("document_id", doc);
        let info: api::DownloadInfo = self.get(url)?;
        let token = info
            .data_token
            .or(info.package_token)
            .filter(|token| !token.url.is_empty())
            .ok_or(Error::InvalidResponse("no download url"))?;
        Url::parse(&token.url).map_err(|_| Error::InvalidResponse("download url"))
    }

    /// `GET` a signed content URL, following Apple's 330 (and ordinary
    /// redirects) once. Cookies go only to iCloud hosts, as a browser would
    /// send them.
    fn content_get(&self, url: Url, range: &Range<u64>) -> Result<Response> {
        let mut url = url;
        for _ in 0..2 {
            let mut request = self.inner.http.get(url.clone()).header(
                "Range",
                format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
            );
            if is_icloud_host(&url) {
                request = request.header("Cookie", self.session()?.cookie.as_str());
            }
            let response = request.send().map_err(|_| Error::Offline)?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308 | 330)
                && let Some(next) = http::header(response.headers(), "location")
                    .and_then(|location| url.join(location).ok())
            {
                url = next;
                continue;
            }
            return Err(self.status_error(status, response.headers()));
        }
        Err(Error::InvalidResponse("redirect loop"))
    }

    /// The `t=` token of the `X-APPLE-WEBAUTH-VALIDATE` cookie, which
    /// `upload/web` wants as a query parameter.
    fn validate_token(&self) -> Result<Option<String>> {
        let session = self.session()?;
        let cookies = Cookies::from_header(&session.cookie);
        Ok(cookies.get("X-APPLE-WEBAUTH-VALIDATE").and_then(|value| {
            value
                .trim_matches('"')
                .split(':')
                .find_map(|part| part.strip_prefix("t="))
                .map(ToOwned::to_owned)
        }))
    }
}

/// A write whose outcome Apple did not report.
fn is_unknown(item: &DriveItem) -> bool {
    item.status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case(UNKNOWN_STATUS))
}

/// Map a per-item status to an error.
fn status_result(item: &DriveItem) -> Result<()> {
    match item.status.as_deref() {
        None | Some("OK") => Ok(()),
        Some("ETAG_CONFLICT") => Err(Error::Conflict {
            current: item.etag.clone().map(Revision::new),
        }),
        Some("NOT_FOUND" | "ITEM_NOT_FOUND" | "ID_INVALID" | "INVALID_ID") => Err(Error::NotFound),
        Some("NAME_ALREADY_EXISTS" | "ALREADY_EXISTS" | "DUPLICATE_NAME") => {
            Err(Error::AlreadyExists)
        }
        Some("QUOTA_EXCEEDED") => Err(Error::InsufficientStorage),
        Some(_) => Err(Error::InvalidResponse("drive item status")),
    }
}

/// An item drivews knows, or [`Error::NotFound`] for its not-found markers
/// and for items in the trash.
fn present(item: DriveItem) -> Result<DriveItem> {
    if item.kind.is_none() {
        return Err(Error::NotFound);
    }
    status_result(&item)?;
    if item.parent_id.as_deref() == Some(api::TRASH_ID) {
        return Err(Error::NotFound);
    }
    Ok(item)
}

/// Names compare after NFC and case folding: iCloud Drive is
/// case-insensitive and returns decomposed Unicode.
fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(Error::InvalidResponse("invalid item name"));
    }
    Ok(())
}

fn is_icloud_host(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "icloud.com" || host.ends_with(".icloud.com"))
}

fn read_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    http::read_json(response).map_err(|error| match error {
        http::BodyError::TooLarge => Error::TooLarge,
        http::BodyError::Read => Error::Offline,
        http::BodyError::Json => Error::InvalidResponse("json"),
    })
}

/// `base` + `/path`, keeping `base`'s own path.
fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let prefix = base.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}/{}", path.trim_start_matches('/')));
    url.set_query(None);
    url
}

/// Errors when a body ends before its declared length, so a cut-off
/// download never passes for a short file.
struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let limit = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let read = self.inner.read(buf.get_mut(..limit).unwrap_or_default())?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the download ended early",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

/// `bytes a-b/total` of a `Content-Range`.
fn content_range(headers: &HeaderMap) -> Option<(Range<u64>, Option<u64>)> {
    let value = http::header(headers, "content-range")?;
    let spec = value.trim().strip_prefix("bytes ")?;
    let (span, total) = spec.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    Some((start..end.saturating_add(1), total.trim().parse().ok()))
}

impl CloudSource for ICloudDrive {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            support: Support::BestEffort,
            change_feed: ChangeFeed::Poll,
            notify: Notify::None,
            recursive_folder_revisions: false,
            revisions: false,
            hashes: Vec::new(),
            share_links: false,
            quota: false,
            trash: true,
            resumable_upload: false,
            upload_chunk: u64::MAX,
            range_reads: true,
            conditional_writes: true,
            multiple_parents: false,
            duplicate_names: false,
            case_insensitive: true,
            native_documents: false,
        }
    }

    fn root(&self) -> ItemId {
        self.inner.config.root.clone()
    }

    fn stat(&self, id: &ItemId) -> Result<Item> {
        let item = self.details(id)?;
        Ok(self.item(item, None))
    }

    fn list(&self, folder: &ItemId) -> Result<Vec<Item>> {
        self.list_folders(std::slice::from_ref(folder))?
            .into_iter()
            .next()
            .map(|(_, children)| children)
            .ok_or(Error::NotFound)
    }

    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        let item = self.stat(id)?;
        if !item.is_file() {
            return Err(Error::NotFound);
        }
        if let Some(expected) = expect
            && item.revision.as_ref() != Some(expected)
        {
            return Err(Error::Conflict {
                current: item.revision,
            });
        }
        let size = item.size.unwrap_or(0);
        let wanted = match range {
            Some(range) => range.start.min(size)..range.end.min(size).max(range.start.min(size)),
            None => 0..size,
        };
        if wanted.is_empty() {
            return Ok(Download {
                body: Box::new(std::io::empty()),
                range: wanted,
                total: Some(size),
                revision: item.revision,
            });
        }
        let url = self.download_url(id)?;
        let response = self.content_get(url, &wanted)?;
        let partial = response.status() == StatusCode::PARTIAL_CONTENT;
        let served = content_range(response.headers());
        let mut body: Box<dyn Read + Send> = Box::new(response);
        if partial {
            let (got, _) = served.ok_or(Error::InvalidResponse("content-range"))?;
            if got.start != wanted.start || got.end < wanted.end {
                return Err(Error::InvalidResponse("content-range"));
            }
        } else if wanted.start > 0 {
            // The whole body came back: skip to the range.
            let skipped = std::io::copy(&mut (&mut body).take(wanted.start), &mut std::io::sink())?;
            if skipped != wanted.start {
                return Err(Error::InvalidResponse("short body"));
            }
        }
        let len = wanted.end.saturating_sub(wanted.start);
        Ok(Download {
            body: Box::new(Exact {
                inner: body.take(len),
                remaining: len,
            }),
            range: wanted,
            total: Some(size),
            revision: item.revision,
        })
    }

    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>> {
        upload::Upload::begin(self, target, size)
            .map(|session| Box::new(session) as Box<dyn UploadSession>)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item> {
        valid_name(name)?;
        if self.find_child(parent, name)?.is_some() {
            return Err(Error::AlreadyExists);
        }
        let body = json!({
            "destinationDrivewsId": parent.as_str(),
            "folders": [{
                "clientId": format!("FOLDER::UNKNOWN_ZONE::TempId-{}", uuid::Uuid::new_v4()),
                "name": name,
            }],
        });
        let answer: api::Folders = self.post(&Service::Drive, "createFolders", &body)?;
        let created = answer
            .folders
            .into_iter()
            .next()
            .ok_or(Error::InvalidResponse("empty createFolders answer"))?;
        if is_unknown(&created) {
            return self
                .find_child(parent, name)?
                .filter(Item::is_folder)
                .ok_or(Error::InvalidResponse("the folder was not created"));
        }
        status_result(&created)?;
        if created.drivewsid.is_empty() || created.etag.is_none() || !created.is_folder() {
            return self
                .find_child(parent, name)?
                .ok_or(Error::InvalidResponse("the folder was not created"));
        }
        Ok(self.item(created, Some(parent)))
    }

    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item> {
        valid_name(new_name)?;
        let current = self.stat(id)?;
        if let Some(expected) = expect
            && current.revision.as_ref() != Some(expected)
        {
            return Err(Error::Conflict {
                current: current.revision,
            });
        }
        let parent = current.parents.first().cloned().unwrap_or_default();
        if &parent == new_parent && current.name == new_name {
            return Ok(current);
        }
        if let Some(taken) = self.find_child(new_parent, new_name)?
            && &taken.id != id
        {
            return Err(Error::AlreadyExists);
        }
        let mut id = id.clone();
        let mut etag = current.revision.map(|r| r.0).unwrap_or_default();
        if &parent != new_parent {
            let body = json!({
                "destinationDrivewsId": new_parent.as_str(),
                "items": [{"drivewsid": id.as_str(), "etag": etag, "clientId": id.as_str()}],
            });
            if let Some(moved) = self.write_item("moveItems", &body)? {
                if !moved.drivewsid.is_empty() {
                    id = ItemId::new(moved.drivewsid);
                }
                if let Some(next) = moved.etag {
                    etag = next;
                }
            } else {
                etag = self.stat(&id)?.revision.map(|r| r.0).unwrap_or_default();
            }
        }
        if current.name != new_name {
            self.rename(&id, &etag, new_name)?;
        }
        let moved = self.stat(&id)?;
        if !moved.parents.contains(new_parent) || fold(&moved.name) != fold(new_name) {
            return Err(Error::InvalidResponse("the move did not take effect"));
        }
        Ok(moved)
    }

    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()> {
        let current = self.stat(id)?;
        if let Some(expected) = expect
            && current.revision.as_ref() != Some(expected)
        {
            return Err(Error::Conflict {
                current: current.revision,
            });
        }
        let etag = current.revision.map(|r| r.0).unwrap_or_default();
        self.trash(id, &etag)
    }
}

/// The current time as docws wants it.
fn now_millis() -> u64 {
    time::millis(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_split_into_type_zone_and_document() {
        assert_eq!(
            split_id("FILE::com.apple.CloudDocs::ABC-1"),
            ("FILE", "com.apple.CloudDocs", "ABC-1")
        );
        assert_eq!(split_id("root"), ("", api::DEFAULT_ZONE, "root"));
    }

    #[test]
    fn names_fold_decomposed_unicode_and_case() {
        assert_eq!(fold("Cafe\u{301}.TXT"), fold("café.txt"));
    }

    #[test]
    fn content_ranges_parse() {
        let mut headers = HeaderMap::new();
        headers.insert("content-range", "bytes 10-19/100".parse().unwrap());
        assert_eq!(content_range(&headers), Some((10..20, Some(100))));
    }

    #[test]
    fn only_icloud_hosts_get_cookies() {
        assert!(is_icloud_host(
            &Url::parse("https://p1-docws.icloud.com/x").unwrap()
        ));
        assert!(!is_icloud_host(
            &Url::parse("https://cvws.icloud-content.com/x").unwrap()
        ));
        assert!(!is_icloud_host(
            &Url::parse("https://icloud.com.evil.example/").unwrap()
        ));
    }

    #[test]
    fn short_bodies_are_errors() {
        let mut exact = Exact {
            inner: std::io::Cursor::new(vec![1_u8; 5]),
            remaining: 10,
        };
        let mut out = Vec::new();
        assert!(exact.read_to_end(&mut out).is_err());
    }
}
