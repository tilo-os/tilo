//! Google Drive accounts as a [`CloudSource`], over the Drive API v3.
//!
//! - **Ids and parents.** Items are Drive file ids. Drive allows several
//!   items with one name in a folder and (for items created before 2020)
//!   several parents; both are passed through as they are and made
//!   presentable by [`cloud_source::tree::Tree`]. When the mounted root is
//!   the whole of My Drive, its id is the `root` alias and every real root id
//!   in `parents` is mapped onto it.
//! - **Change feed.** [`CloudSource::changes`] wraps `changes.list`. Drive's
//!   feed is account-wide and has no "list everything" mode, so the cursor
//!   carries a start page token plus a queue of folders still to enumerate:
//!   a fresh cursor enumerates the root breadth-first (several folders per
//!   `files.list` query) and then continues from the token taken before the
//!   walk began. Changes outside the mounted subtree are reported as
//!   removals (they may have been moved out), and a folder that enters the
//!   subtree is enumerated because Drive reports only the folder itself.
//! - **No push without a public endpoint.** `changes.watch` delivers to an
//!   HTTPS webhook reachable from Google, which a desktop cannot offer, so
//!   [`Capabilities::notify`] is [`Notify::None`]. [`CloudSource::wait`]
//!   still works: it polls one-entry `changes.list` pages every
//!   [`DriveConfig::poll_interval`] ([`DEFAULT_POLL_INTERVAL`], 30 s) until
//!   something changes or the timeout passes.
//! - **Revisions.** A blob file's revision is its `headRevisionId`, which
//!   moves exactly when the content does and names an entry of
//!   `revisions.list`; folders and native documents use `v<version>`.
//! - **Conditional writes are checked, not atomic.** Drive v3 has no
//!   `If-Match` on `files.update`. A write with an expected revision checks
//!   `headRevisionId` when the upload starts and again immediately before
//!   the request that commits it; the commit request carries at most
//!   [`UPLOAD_QUANTUM`] bytes so the window between check and commit is one
//!   round trip plus that chunk. A write that lands inside the window is
//!   detected afterwards from the revision history: the overwritten
//!   revisions are pinned with `keepForever` and the write reports
//!   [`Error::Conflict`]. Moves and deletes check `version`/`headRevisionId`
//!   first with the same one-round-trip window (a delete goes to the trash
//!   by default, so it stays recoverable). Because the window cannot be
//!   closed, [`Capabilities::conditional_writes`] is `false`.
//! - **Native documents.** Docs, Sheets, Slides, Drawings (and Forms, Sites,
//!   shortcuts) are [`ItemKind::Native`] with their `webViewLink`; Docs,
//!   Sheets, Slides and Drawings export to office formats and PDF.
//! - **Rate limits.** `403 userRateLimitExceeded`/`rateLimitExceeded` and
//!   `429` are [`Error::RateLimited`] with the `Retry-After` delay when the
//!   server sent one. Nothing is retried here; the daemon schedules retries
//!   and chooses the backoff.
//! - **Credentials.** Every request asks the [`CredentialSource`] for a
//!   Bearer token; a 401 reports [`CredentialSource::rejected`] so Settings
//!   refreshes it.

#![allow(
    clippy::missing_errors_doc,
    reason = "every fallible method returns the provider-neutral `cloud_source::Error`"
)]

pub mod api;
#[cfg(feature = "fake-server")]
pub mod fake;
mod feed;
mod upload;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use cloud_source::{
    Capabilities, ChangeFeed, ChangePage, CloudSource, Credential, CredentialSource, Cursor,
    Download, Error, ExportFormat, HashAlgorithm, Item, ItemId, Notify, Quota, Result, Revision,
    RevisionInfo, Support, UploadSession, UploadState, UploadTarget, Wait, http,
};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use reqwest::Url;

use crate::api::{About, ErrorBody, File, FileList, RevisionEntry, RevisionList};

/// Google's API origin.
pub const DEFAULT_API: &str = "https://www.googleapis.com/";
/// Resumable upload chunks must be multiples of this (except the last).
pub const UPLOAD_QUANTUM: u64 = 256 * 1024;
/// Bytes per upload request: 32 quanta.
pub const DEFAULT_UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;
/// How often [`CloudSource::wait`] polls the changes feed.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

const ERROR_BODY_LIMIT: usize = 64 * 1024;
/// A 1000-entry listing page is around a megabyte.
const JSON_LIMIT: usize = 32 * 1024 * 1024;
/// Drive refuses exports over 10 MB (`exportSizeLimitExceeded`).
const EXPORT_LIMIT: usize = 16 * 1024 * 1024;
const PAGE_SIZE: &str = "1000";
const JSON_UTF8: &str = "application/json; charset=UTF-8";

/// Which part of which Drive a source mounts. Contains no secret: it is the
/// non-secret mount configuration a daemon persists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriveConfig {
    /// API origin with a trailing slash; [`DEFAULT_API`] except in tests.
    pub api: String,
    /// The folder to mount; the whole of My Drive (or of the shared drive)
    /// when `None`.
    pub root: Option<String>,
    /// A shared drive to mount instead of My Drive.
    pub shared_drive: Option<String>,
    /// Delete moves to the Drive trash (recoverable for 30 days) instead of
    /// deleting permanently.
    pub trash: bool,
    /// Bytes per upload request, a multiple of [`UPLOAD_QUANTUM`].
    pub upload_chunk: u64,
    /// Poll interval of [`CloudSource::wait`].
    pub poll_interval: Duration,
}

impl Default for DriveConfig {
    fn default() -> Self {
        Self::my_drive()
    }
}

impl DriveConfig {
    /// The whole of My Drive.
    pub fn my_drive() -> Self {
        Self {
            api: DEFAULT_API.to_owned(),
            root: None,
            shared_drive: None,
            trash: true,
            upload_chunk: DEFAULT_UPLOAD_CHUNK,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// The whole of the shared drive `drive_id`.
    pub fn shared_drive(drive_id: impl Into<String>) -> Self {
        Self {
            shared_drive: Some(drive_id.into()),
            ..Self::my_drive()
        }
    }

    /// Mount only the folder `folder_id`.
    #[must_use]
    pub fn with_root(mut self, folder_id: impl Into<String>) -> Self {
        self.root = Some(folder_id.into());
        self
    }

    /// Talk to `api` (a fake server) instead of Google.
    #[must_use]
    pub fn with_api(mut self, api: impl Into<String>) -> Self {
        let mut api = api.into();
        if !api.ends_with('/') {
            api.push('/');
        }
        self.api = api;
        self
    }
}

/// One Google Drive account (or shared drive, or folder of either).
#[derive(Clone)]
pub struct GDrive {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    config: DriveConfig,
    http: Client,
    credentials: Arc<dyn CredentialSource>,
    /// The real id behind the `root` alias, once asked.
    real_root: Mutex<Option<String>>,
    /// Parents of folders seen so far, to decide whether a change lies
    /// below the root. An empty list means "not below any folder".
    folders: Mutex<HashMap<String, Vec<String>>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl GDrive {
    /// A source for `config` authenticating with `credentials`. No request
    /// is made until the first call.
    pub fn new(mut config: DriveConfig, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let quanta = config.upload_chunk.div_ceil(UPLOAD_QUANTUM).max(1);
        config.upload_chunk = quanta.saturating_mul(UPLOAD_QUANTUM);
        let http = tilo_net::http::blocking_client()
            .user_agent("Tilo-Storage/0.1")
            // Whole-request deadline: chunk uploads and large ranged reads
            // stream for a while; the connect timeout catches dead links.
            .timeout(Duration::from_secs(30 * 60))
            .connect_timeout(Duration::from_secs(10))
            // A resumable upload's `308 Resume Incomplete` is not a
            // redirect, and nothing Drive answers needs following.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Offline)?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                http,
                credentials,
                real_root: Mutex::new(None),
                folders: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn config(&self) -> &DriveConfig {
        &self.inner.config
    }

    /// Create an empty Google-native document (`mime` is one of the
    /// `application/vnd.google-apps.*` types) in `parent`.
    pub fn create_native_document(&self, parent: &ItemId, name: &str, mime: &str) -> Result<Item> {
        if !mime.starts_with("application/vnd.google-apps.") || mime == api::FOLDER_MIME {
            return Err(Error::Unsupported("not a Google-native document type"));
        }
        self.inner.create(parent, name, mime)
    }
}

impl Inner {
    fn configured_root(&self) -> Option<&str> {
        self.config
            .root
            .as_deref()
            .or(self.config.shared_drive.as_deref())
    }

    fn root_id(&self) -> ItemId {
        ItemId::new(self.configured_root().unwrap_or("root"))
    }

    /// The real id of the root folder (resolving the `root` alias once).
    pub(crate) fn real_root(&self) -> Result<String> {
        if let Some(root) = self.configured_root() {
            return Ok(root.to_owned());
        }
        let cached = lock(&self.real_root).clone();
        if let Some(root) = cached {
            return Ok(root);
        }
        let file = self.get_file("root", "id")?;
        *lock(&self.real_root) = Some(file.id.clone());
        Ok(file.id)
    }

    /// The Drive id for `id`, resolving the `root` alias.
    fn real_id(&self, id: &ItemId) -> Result<String> {
        if self.configured_root().is_none() && id.as_str() == "root" {
            self.real_root()
        } else {
            Ok(id.0.clone())
        }
    }

    pub(crate) fn to_item(&self, file: &File) -> Result<Item> {
        let root = self.root_id();
        let real = self.real_root()?;
        let mut item = file.to_item(|parent| {
            if parent == real {
                root.clone()
            } else {
                ItemId::new(parent)
            }
        });
        if file.id == real || file.id == root.0 {
            item.id = root;
            item.parents.clear();
        }
        if file.is_folder() {
            self.remember_folder(&file.id, file.parents.clone());
        }
        Ok(item)
    }

    pub(crate) fn remember_folder(&self, id: &str, parents: Vec<String>) {
        lock(&self.folders).insert(id.to_owned(), parents);
    }

    // --- HTTP -------------------------------------------------------------

    pub(crate) fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<Url> {
        let mut url =
            Url::parse(&self.config.api).map_err(|_| Error::InvalidResponse("api url"))?;
        url.path_segments_mut()
            .map_err(|()| Error::InvalidResponse("api url"))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// `true` when `url` is on the API origin, so it may see the token.
    pub(crate) fn same_origin(&self, url: &Url) -> bool {
        Url::parse(&self.config.api).is_ok_and(|api| api.origin() == url.origin())
    }

    pub(crate) fn request(&self, method: Method, url: Url) -> Result<RequestBuilder> {
        match self.credentials.credential()? {
            Credential::Bearer { token, .. } => {
                Ok(self.http.request(method, url).bearer_auth(token.as_str()))
            }
            Credential::Basic { .. } | Credential::Session { .. } => Err(Error::Unsupported(
                "Google Drive needs an OAuth access token",
            )),
        }
    }

    pub(crate) fn send(builder: RequestBuilder) -> Result<Response> {
        builder.send().map_err(|_| Error::Offline)
    }

    pub(crate) fn check(&self, response: Response) -> Result<Response> {
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(self.failure(response, false))
        }
    }

    /// The error for a failed response. With `paged`, an error about the
    /// page token of a cursor means the cursor expired.
    pub(crate) fn failure(&self, response: Response, paged: bool) -> Error {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| http::parse_retry_after(v, SystemTime::now()));
        let body = read_bounded(response, ERROR_BODY_LIMIT).unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED {
            return http::unauthorized(self.credentials.as_ref());
        }
        if paged
            && (status == StatusCode::GONE
                || (matches!(status.as_u16(), 400 | 404)
                    && ErrorBody::parse(&body).is_page_token_error()))
        {
            return Error::CursorExpired;
        }
        api::classify(status.as_u16(), retry_after, &body)
    }

    pub(crate) fn call(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<Response> {
        let mut builder = self.request(method, self.url(segments, query)?)?;
        if let Some(body) = body {
            builder = builder
                .header(CONTENT_TYPE, JSON_UTF8)
                .body(serde_json::to_vec(body).map_err(|_| Error::InvalidResponse("json"))?);
        }
        self.check(Self::send(builder)?)
    }

    pub(crate) fn json<T: DeserializeOwned>(response: Response) -> Result<T> {
        let bytes = read_bounded(response, JSON_LIMIT)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse("json"))
    }

    pub(crate) fn get_file(&self, id: &str, fields: &str) -> Result<File> {
        Self::json(self.call(
            Method::GET,
            &["drive", "v3", "files", id],
            &[("fields", fields), ("supportsAllDrives", "true")],
            None,
        )?)
    }

    /// The file, failing with [`Error::NotFound`] when it is trashed.
    fn live_file(&self, id: &ItemId, fields: &str) -> Result<File> {
        let file = self.get_file(&self.real_id(id)?, fields)?;
        if file.trashed {
            return Err(Error::NotFound);
        }
        Ok(file)
    }

    /// Check `expect` against the file's current revision.
    pub(crate) fn check_revision(&self, id: &ItemId, expect: Option<&Revision>) -> Result<File> {
        let file = self.live_file(id, api::REVISION_FIELDS)?;
        if let Some(expect) = expect {
            let current = file.revision();
            if current.as_ref() != Some(expect) {
                return Err(Error::Conflict { current });
            }
        }
        Ok(file)
    }

    /// Query parameters that scope listings and the feed to the mounted
    /// drive.
    pub(crate) fn scope(&self) -> Vec<(&'static str, String)> {
        let mut scope = vec![
            ("supportsAllDrives", "true".to_owned()),
            ("includeItemsFromAllDrives", "true".to_owned()),
        ];
        if let Some(drive) = &self.config.shared_drive {
            scope.push(("corpora", "drive".to_owned()));
            scope.push(("driveId", drive.clone()));
        }
        scope
    }

    /// One `files.list` page for `query`.
    pub(crate) fn list_page(&self, query: &str, page: Option<&str>) -> Result<FileList> {
        let fields = format!("nextPageToken,files({})", api::FILE_FIELDS);
        let mut params = self.scope();
        params.push(("q", query.to_owned()));
        params.push(("fields", fields));
        params.push(("pageSize", PAGE_SIZE.to_owned()));
        if let Some(page) = page {
            params.push(("pageToken", page.to_owned()));
        }
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = self.url(&["drive", "v3", "files"], &params)?;
        let response = Self::send(self.request(Method::GET, url)?)?;
        if !response.status().is_success() {
            return Err(self.failure(response, page.is_some()));
        }
        Self::json(response)
    }

    fn create(&self, parent: &ItemId, name: &str, mime: &str) -> Result<Item> {
        let parent = self.real_id(parent)?;
        let body = serde_json::json!({ "name": name, "mimeType": mime, "parents": [parent] });
        let file: File = Self::json(self.call(
            Method::POST,
            &["drive", "v3", "files"],
            &[("fields", api::FILE_FIELDS), ("supportsAllDrives", "true")],
            Some(&body),
        )?)?;
        self.to_item(&file)
    }

    /// Stream `range` of `url`, a media download.
    pub(crate) fn download(
        &self,
        url: Url,
        range: Option<Range<u64>>,
        revision: Option<Revision>,
    ) -> Result<Download> {
        if let Some(range) = range.as_ref().filter(|r| r.start >= r.end) {
            return Ok(Download {
                body: Box::new(std::io::empty()),
                range: range.start..range.start,
                total: None,
                revision,
            });
        }
        let mut builder = self.request(Method::GET, url)?;
        if let Some(range) = &range {
            builder = builder.header(
                RANGE,
                format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
            );
        }
        let response = self.check(Self::send(builder)?)?;
        let served = http::Served {
            partial: response.status() == StatusCode::PARTIAL_CONTENT,
            content_range: response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(http::parse_content_range),
            declared: response.content_length(),
        };
        let download = http::ranged_body(Box::new(response), range, served)?;
        Ok(Download {
            revision,
            ..download
        })
    }

    fn revision_media_url(&self, id: &str, revision: &str) -> Result<Url> {
        self.url(
            &["drive", "v3", "files", id, "revisions", revision],
            &[("alt", "media"), ("supportsAllDrives", "true")],
        )
    }

    fn pin_revision(&self, id: &str, revision: &str) -> Result<()> {
        self.call(
            Method::PATCH,
            &["drive", "v3", "files", id, "revisions", revision],
            &[("fields", "id,keepForever")],
            Some(&serde_json::json!({ "keepForever": true })),
        )
        .map(drop)
    }

    /// Revision ids of `id`, oldest first.
    pub(crate) fn revision_ids(&self, id: &str) -> Result<Vec<RevisionEntry>> {
        let mut all = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut params = vec![
                (
                    "fields",
                    "nextPageToken,revisions(id,modifiedTime,size,keepForever,lastModifyingUser(displayName,emailAddress))",
                ),
                ("pageSize", PAGE_SIZE),
            ];
            if let Some(page) = &page {
                params.push(("pageToken", page.as_str()));
            }
            let list: RevisionList = Self::json(self.call(
                Method::GET,
                &["drive", "v3", "files", id, "revisions"],
                &params,
                None,
            )?)?;
            all.extend(list.revisions);
            match list.next_page_token {
                Some(next) => page = Some(next),
                None => return Ok(all),
            }
        }
    }

    /// After a conditional write committed `new_head`, make sure nothing
    /// landed between `expect` and it. Revisions that did are pinned so
    /// they survive Drive's revision pruning, and the write is reported as
    /// a conflict.
    pub(crate) fn verify_commit(&self, id: &str, expect: &Revision, new_head: &str) -> Result<()> {
        let ids: Vec<String> = self
            .revision_ids(id)?
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        let (Some(head), Some(base)) = (
            ids.iter().position(|r| r == new_head),
            ids.iter().position(|r| r == expect.as_str()),
        ) else {
            // The base was pruned or the history is not visible: nothing to
            // compare against.
            return Ok(());
        };
        let between = ids.get(base.saturating_add(1)..head).unwrap_or_default();
        if between.is_empty() {
            return Ok(());
        }
        for revision in between {
            // Best effort: the conflict is reported either way.
            let _ = self.pin_revision(id, revision);
        }
        Err(Error::Conflict {
            current: Some(Revision::new(new_head)),
        })
    }
}

/// Read at most `limit` bytes of a body.
pub(crate) fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    let declared = response.content_length();
    http::read_bounded(response, declared, limit)
}

impl CloudSource for GDrive {
    fn capabilities(&self) -> Capabilities {
        let shared = self.inner.config.shared_drive.is_some();
        Capabilities {
            support: Support::Official,
            change_feed: ChangeFeed::Cursor,
            notify: Notify::None,
            recursive_folder_revisions: false,
            revisions: true,
            hashes: vec![
                HashAlgorithm::Md5,
                HashAlgorithm::Sha1,
                HashAlgorithm::Sha256,
            ],
            share_links: true,
            // Shared drive storage is not the person's quota.
            quota: !shared,
            trash: self.inner.config.trash,
            resumable_upload: true,
            upload_chunk: self.inner.config.upload_chunk,
            range_reads: true,
            // Checked immediately before the commit, not atomically; see the
            // crate documentation for the window.
            conditional_writes: false,
            // Legacy My Drive items; shared drives enforce one parent.
            multiple_parents: !shared,
            duplicate_names: true,
            case_insensitive: false,
            native_documents: true,
        }
    }

    fn root(&self) -> ItemId {
        self.inner.root_id()
    }

    fn stat(&self, id: &ItemId) -> Result<Item> {
        let file = self.inner.live_file(id, api::FILE_FIELDS)?;
        self.inner.to_item(&file)
    }

    fn list(&self, folder: &ItemId) -> Result<Vec<Item>> {
        let inner = &self.inner;
        let query = format!(
            "{} in parents and trashed = false",
            api::quote(&inner.real_id(folder)?)
        );
        let mut items = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let list = inner.list_page(&query, page.as_deref())?;
            for file in &list.files {
                items.push(inner.to_item(file)?);
            }
            match list.next_page_token {
                Some(next) => page = Some(next),
                None => return Ok(items),
            }
        }
    }

    fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        self.inner.changes(cursor)
    }

    fn wait(&self, cursor: Option<&Cursor>, timeout: Duration) -> Result<Wait> {
        self.inner.wait(cursor, timeout)
    }

    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        if let Some(expect) = expect {
            let file = inner.check_revision(id, Some(expect))?;
            if file.is_native() || file.is_folder() {
                return Err(Error::Unsupported("reading an item without a byte body"));
            }
            if file.head_revision_id.as_deref() == Some(expect.as_str()) {
                // Download that exact revision, so the bytes cannot belong
                // to a newer one even if the file changes meanwhile.
                let url = inner.revision_media_url(&real, expect.as_str())?;
                match inner.download(url, range.clone(), Some(expect.clone())) {
                    Err(Error::Forbidden) => {}
                    other => return other,
                }
            }
        }
        let url = inner.url(
            &["drive", "v3", "files", &real],
            &[("alt", "media"), ("supportsAllDrives", "true")],
        )?;
        inner.download(url, range, None)
    }

    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>> {
        upload::begin(&self.inner, target, size)
    }

    fn resume_upload(&self, state: &UploadState) -> Result<Box<dyn UploadSession>> {
        upload::resume(&self.inner, state)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item> {
        self.inner.create(parent, name, api::FOLDER_MIME)
    }

    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        let current = inner.check_revision(id, expect)?;
        let target = inner.real_id(new_parent)?;
        let mut params = vec![
            ("fields", api::FILE_FIELDS.to_owned()),
            ("supportsAllDrives", "true".to_owned()),
        ];
        if !current.parents.contains(&target) {
            // A move leaves every old parent: the trait has no notion of
            // moving one of several parent links.
            params.push(("addParents", target));
            if !current.parents.is_empty() {
                params.push(("removeParents", current.parents.join(",")));
            }
        }
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let file: File = Inner::json(inner.call(
            Method::PATCH,
            &["drive", "v3", "files", &real],
            &params,
            Some(&serde_json::json!({ "name": new_name })),
        )?)?;
        inner.to_item(&file)
    }

    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        inner.check_revision(id, expect)?;
        if inner.config.trash {
            inner
                .call(
                    Method::PATCH,
                    &["drive", "v3", "files", &real],
                    &[("fields", "id,trashed"), ("supportsAllDrives", "true")],
                    Some(&serde_json::json!({ "trashed": true })),
                )
                .map(drop)
        } else {
            inner
                .call(
                    Method::DELETE,
                    &["drive", "v3", "files", &real],
                    &[("supportsAllDrives", "true")],
                    None,
                )
                .map(drop)
        }
    }

    fn revisions(&self, id: &ItemId) -> Result<Vec<RevisionInfo>> {
        let real = self.inner.real_id(id)?;
        let mut revisions: Vec<RevisionInfo> = self
            .inner
            .revision_ids(&real)?
            .into_iter()
            .map(|entry| RevisionInfo {
                revision: Revision::new(entry.id),
                modified: entry.modified_time.as_deref().and_then(http::parse_rfc3339),
                size: entry.size,
                author: entry
                    .last_modifying_user
                    .and_then(|user| user.display_name.or(user.email_address)),
            })
            .collect();
        revisions.reverse();
        Ok(revisions)
    }

    fn read_revision(
        &self,
        id: &ItemId,
        revision: &Revision,
        range: Option<Range<u64>>,
    ) -> Result<Download> {
        let url = self
            .inner
            .revision_media_url(&self.inner.real_id(id)?, revision.as_str())?;
        self.inner.download(url, range, Some(revision.clone()))
    }

    fn restore_revision(&self, id: &ItemId, revision: &Revision) -> Result<Item> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        let entry: RevisionEntry = Inner::json(inner.call(
            Method::GET,
            &[
                "drive",
                "v3",
                "files",
                &real,
                "revisions",
                revision.as_str(),
            ],
            &[("fields", "id,size")],
            None,
        )?)?;
        let size = entry.size.ok_or(Error::InvalidResponse("revision size"))?;
        let body = self.read_revision(id, revision, None)?.body;
        upload::restore(inner, id, size, body)
    }

    fn export(&self, id: &ItemId, format: &ExportFormat) -> Result<Download> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        let response = inner.call(
            Method::GET,
            &["drive", "v3", "files", &real, "export"],
            &[("mimeType", format.mime.as_str())],
            None,
        )?;
        // Exports are capped at 10 MB server-side; buffering gives the
        // exact length the caller is promised.
        let bytes = read_bounded(response, EXPORT_LIMIT)?;
        let len = bytes.len() as u64;
        Ok(Download {
            body: Box::new(std::io::Cursor::new(bytes)),
            range: 0..len,
            total: Some(len),
            revision: None,
        })
    }

    fn share_link(&self, id: &ItemId) -> Result<String> {
        let inner = &self.inner;
        let real = inner.real_id(id)?;
        inner.call(
            Method::POST,
            &["drive", "v3", "files", &real, "permissions"],
            &[("fields", "id"), ("supportsAllDrives", "true")],
            Some(&serde_json::json!({
                "type": "anyone",
                "role": "reader",
                "allowFileDiscovery": false,
            })),
        )?;
        inner
            .get_file(&real, "id,webViewLink")?
            .web_view_link
            .ok_or(Error::InvalidResponse("webViewLink"))
    }

    fn quota(&self) -> Result<Quota> {
        if self.inner.config.shared_drive.is_some() {
            return Err(Error::Unsupported("quota of a shared drive"));
        }
        let about: About = Inner::json(self.inner.call(
            Method::GET,
            &["drive", "v3", "about"],
            &[("fields", "storageQuota(limit,usage)")],
            None,
        )?)?;
        Ok(Quota {
            used: about.storage_quota.usage.unwrap_or(0),
            total: about.storage_quota.limit,
        })
    }
}

impl std::fmt::Debug for GDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GDrive")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud_source::StaticCredential;

    #[test]
    fn upload_chunks_round_up_to_the_quantum() {
        let credentials = Arc::new(StaticCredential(Credential::Bearer {
            token: String::from("t").into(),
            expires: None,
        }));
        let config = DriveConfig {
            upload_chunk: 300_000,
            ..DriveConfig::my_drive()
        };
        let drive = GDrive::new(config, credentials).unwrap();
        assert_eq!(drive.capabilities().upload_chunk, 2 * UPLOAD_QUANTUM);
        assert_eq!(drive.root(), ItemId::new("root"));
        let shared = GDrive::new(
            DriveConfig::shared_drive("0AdrIvE"),
            Arc::new(StaticCredential(Credential::Bearer {
                token: String::from("t").into(),
                expires: None,
            })),
        )
        .unwrap();
        assert_eq!(shared.root(), ItemId::new("0AdrIvE"));
        assert!(!shared.capabilities().multiple_parents);
        assert!(!shared.capabilities().quota);
    }

    #[test]
    fn configurations_persist_with_defaults() {
        let config: DriveConfig = serde_json::from_str(r#"{"root":"1fOlDeR"}"#).unwrap();
        assert_eq!(config, DriveConfig::my_drive().with_root("1fOlDeR"));
        let round = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<DriveConfig>(&round).unwrap(), config);
    }
}
