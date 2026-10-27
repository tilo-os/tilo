//! Dropbox accounts as a [`cloud_source::CloudSource`], on API v2.
//!
//! - **Identity.** Items are Dropbox ids (`id:…`); the mounted root is
//!   [`DropboxSource::root`] (`""`), which is the configured
//!   [`DropboxConfig::root`] path. Dropbox addresses by case-insensitive
//!   path (`path_lower` for lookups, `name`/`path_display` for display) and
//!   reports no parent ids, so the source keeps a path↔id map to name
//!   parents and to attribute `deleted` entries, which carry only a path.
//! - **Change feed.** `files/list_folder` (recursive) starts an enumeration,
//!   `list_folder/continue` pages through changes; an expired cursor
//!   (`reset`) restarts the enumeration as a reset page.
//!   `list_folder/longpoll` is [`CloudSource::wait`], and a `backoff` it asks
//!   for is honoured: the next wait returns [`Wait::Backoff`] without a
//!   request until it has passed.
//! - **Integrity.** Every file carries its `content_hash` (4 MiB block
//!   SHA-256); whole downloads are verified against it and uploads send it,
//!   so Dropbox rejects bytes damaged in transit, and are checked against
//!   what Dropbox committed.
//! - **Writes.** Replacing with a known revision uses `WriteMode::update`,
//!   so a stale write is a conflict, never an overwrite; creates use `add`
//!   with `strict_conflict`. Files larger than one chunk go through upload
//!   sessions (`start`/`append_v2`/`finish`) that survive a restart.
//!   Moves and deletes check the revision first (`delete_v2` also sends it
//!   as `parent_rev`).
//! - **Team spaces.** [`DropboxSource::team_root`] finds a team member's
//!   root namespace; set it as [`DropboxConfig::path_root`] and every call
//!   carries `Dropbox-API-Path-Root`.
//!
//! Errors never carry tokens, URLs or response bodies. A 401 is reported
//! to the [`CredentialSource`] so Settings refreshes the token.

#![allow(
    clippy::missing_errors_doc,
    reason = "fallible methods return cloud_source::Error, documented there"
)]

use std::io::{self, Read};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cloud_source::{
    Capabilities, Change, ChangeFeed, ChangePage, CloudSource, ContentHash, CredentialSource,
    Cursor, Download, Error, HashAlgorithm, Item, ItemId, ItemKind, Notify, Quota, Result,
    Revision, RevisionInfo, Support, UploadSession, UploadState, UploadTarget, Wait, http,
};
use serde_json::{Value, json};

mod api;
pub mod content_hash;
#[cfg(feature = "fake-server")]
pub mod fake;
mod metadata;
mod time;
mod upload;

pub use api::Endpoints;

use api::{Api, Failure};
use metadata::{Entry, Kind, Paths, is_within, parent_path};
use tilo_lock::MutexRecover as _;

/// Default bytes per upload-session chunk: a multiple of the 4 MiB hash
/// block, so a resumed session can keep verifying the whole file.
pub const DEFAULT_UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;
/// Largest body Dropbox accepts in one upload request.
pub const MAX_UPLOAD_REQUEST: u64 = 150 * 1024 * 1024;
/// Long-poll timeouts Dropbox accepts, in seconds.
const LONGPOLL_SECONDS: std::ops::RangeInclusive<u64> = 30..=480;

/// Where and how to reach one account. Contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DropboxConfig {
    /// The mounted folder: `""` for the whole account, else `/Some/Folder`.
    pub root: String,
    #[serde(default)]
    pub endpoints: Endpoints,
    /// The namespace calls are rooted in (`Dropbox-API-Path-Root`), for a
    /// team member's team space; see [`DropboxSource::team_root`].
    #[serde(default)]
    pub path_root: Option<String>,
    /// Bytes per upload-session chunk; smaller files go in one request.
    pub upload_chunk: u64,
    /// `limit` for `list_folder` pages (Dropbox's default when `None`).
    #[serde(default)]
    pub page_limit: Option<u32>,
}

impl DropboxConfig {
    /// The whole account (`root = ""`) or a folder in it.
    pub fn new(root: &str) -> Self {
        Self {
            root: normalize_root(root),
            endpoints: Endpoints::default(),
            path_root: None,
            upload_chunk: DEFAULT_UPLOAD_CHUNK,
            page_limit: None,
        }
    }
}

fn normalize_root(root: &str) -> String {
    let trimmed = root.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// A mounted Dropbox account. Cloning shares the connection pool, the
/// credential source and the path map.
#[derive(Clone)]
pub struct DropboxSource {
    config: DropboxConfig,
    root_lower: String,
    api: Api,
    paths: Arc<Mutex<Paths>>,
    backoff_until: Arc<Mutex<Option<Instant>>>,
}

/// A Dropbox id (`id:…`); anything else cannot name an item.
fn is_dropbox_id(id: &ItemId) -> bool {
    id.as_str().len() > 3 && id.as_str().starts_with("id:")
}

/// A Dropbox `rev`: at least nine lowercase hex digits. A revision that
/// cannot exist is never current.
fn is_rev(revision: &Revision) -> bool {
    let rev = revision.as_str();
    rev.len() >= 9
        && rev
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(Error::InvalidResponse("file name"));
    }
    Ok(())
}

impl DropboxSource {
    /// A source for `config` authenticating with `credentials` (a Bearer
    /// token). No request is made until the first call.
    pub fn new(config: DropboxConfig, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let mut config = config;
        config.root = normalize_root(&config.root);
        let api = Api::new(
            config.endpoints.clone(),
            credentials,
            config.path_root.clone(),
        )?;
        Ok(Self {
            root_lower: config.root.to_lowercase(),
            config,
            api,
            paths: Arc::new(Mutex::new(Paths::default())),
            backoff_until: Arc::new(Mutex::new(None)),
        })
    }

    pub fn config(&self) -> &DropboxConfig {
        &self.config
    }

    /// The root namespace of a team member whose account has a team space
    /// (`root_namespace_id` differs from `home_namespace_id`); `None` for
    /// individual accounts. Store it as [`DropboxConfig::path_root`].
    pub fn team_root(&self) -> Result<Option<String>> {
        let account = self
            .api
            .rpc("users/get_current_account", &Value::Null)
            .map_err(Failure::into_error)?;
        let info = account
            .get("root_info")
            .ok_or(Error::InvalidResponse("root_info"))?;
        let root = info.get("root_namespace_id").and_then(Value::as_str);
        let home = info.get("home_namespace_id").and_then(Value::as_str);
        Ok(match (root, home) {
            (Some(root), Some(home)) if root != home => Some(root.to_owned()),
            _ => None,
        })
    }

    fn paths(&self) -> MutexGuard<'_, Paths> {
        self.paths.lock_recover()
    }

    fn api_path(&self, id: &ItemId) -> Result<String> {
        if id.as_str().is_empty() {
            Ok(self.config.root.clone())
        } else if is_dropbox_id(id) {
            Ok(id.0.clone())
        } else {
            Err(Error::NotFound)
        }
    }

    fn within_root(&self, path_lower: &str) -> bool {
        path_lower != self.root_lower && is_within(path_lower, &self.root_lower)
    }

    fn list_arg(&self, path: &str, recursive: bool) -> Value {
        let mut arg = json!({
            "path": path,
            "recursive": recursive,
            "include_deleted": false,
            "include_non_downloadable_files": true,
        });
        if let (Some(limit), Some(object)) = (self.config.page_limit, arg.as_object_mut()) {
            object.insert("limit".into(), json!(limit));
        }
        arg
    }

    /// `files/get_metadata` of an id or path.
    fn metadata(&self, path: &str) -> Result<Entry> {
        let value = self
            .api
            .rpc("files/get_metadata", &json!({"path": path}))
            .map_err(Failure::into_error)?;
        Entry::parse(&value).ok_or(Error::InvalidResponse("metadata"))
    }

    /// Metadata of an item under the root.
    fn entry(&self, id: &ItemId) -> Result<Entry> {
        let path = self.api_path(id)?;
        let entry = self.metadata(&path)?;
        match &entry.path_lower {
            Some(lower) if self.within_root(lower) => {
                self.remember(&entry);
                Ok(entry)
            }
            _ => Err(Error::NotFound),
        }
    }

    /// The lowercased path of a folder, for naming children.
    fn folder_path(&self, id: &ItemId) -> Result<String> {
        if id.as_str().is_empty() {
            return Ok(self.config.root.clone());
        }
        let entry = self.entry(id)?;
        if entry.kind() != Kind::Folder {
            return Err(Error::NotFound);
        }
        entry.path_lower.ok_or(Error::InvalidResponse("path_lower"))
    }

    fn remember(&self, entry: &Entry) {
        if let (Some(id), Some(lower)) = (&entry.id, &entry.path_lower)
            && entry.kind() != Kind::Deleted
        {
            self.paths().insert(&ItemId::new(id.clone()), lower);
        }
    }

    fn id_at(&self, path_lower: &str) -> Result<ItemId> {
        if path_lower == self.root_lower {
            return Ok(self.root());
        }
        let known = self.paths().get(path_lower).cloned();
        if let Some(id) = known {
            return Ok(id);
        }
        let entry = self.metadata(path_lower)?;
        self.remember(&entry);
        entry
            .id
            .map(ItemId::new)
            .ok_or(Error::InvalidResponse("id"))
    }

    /// An item from an entry; `parent` when the caller knows it.
    fn item(&self, entry: &Entry, parent: Option<&ItemId>) -> Result<Item> {
        let id = entry.id.clone().ok_or(Error::InvalidResponse("id"))?;
        let lower = entry
            .path_lower
            .as_deref()
            .ok_or(Error::InvalidResponse("path_lower"))?;
        let parent = match parent {
            Some(parent) => parent.clone(),
            None => self.id_at(parent_path(lower))?,
        };
        let file = entry.kind() == Kind::File;
        Ok(Item {
            id: ItemId::new(id),
            parents: vec![parent],
            name: entry.name.clone(),
            kind: if file {
                ItemKind::File
            } else {
                ItemKind::Folder
            },
            size: if file { entry.size } else { None },
            modified: entry.server_modified.as_deref().and_then(time::parse),
            revision: entry.rev.clone().filter(|_| file).map(Revision::new),
            hashes: entry
                .content_hash
                .iter()
                .map(|value| ContentHash {
                    algorithm: HashAlgorithm::DropboxContentHash,
                    value: value.clone(),
                })
                .collect(),
            mime: None,
        })
    }

    fn entries(value: &Value) -> Vec<Entry> {
        value
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Entry::parse).collect())
            .unwrap_or_default()
    }

    fn start_enumeration(&self) -> Result<Value> {
        self.api
            .rpc("files/list_folder", &self.list_arg(&self.config.root, true))
            .map_err(Failure::into_error)
    }

    /// Turn a `list_folder` result into a page; `None` when a deletion
    /// cannot be attributed to an id, so the enumeration must restart.
    fn page(&self, value: &Value, reset: bool) -> Result<Option<ChangePage>> {
        let cursor = value
            .get("cursor")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidResponse("cursor"))?;
        let has_more = value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut changes = Vec::new();
        for entry in Self::entries(value) {
            let Some(lower) = entry.path_lower.clone() else {
                continue;
            };
            if entry.kind() == Kind::Deleted {
                if !self.within_root(&lower) {
                    continue;
                }
                let removed = self.paths().remove_tree(&lower);
                if !removed.is_empty() {
                    changes.extend(removed.into_iter().map(Change::Remove));
                } else if self.paths().take_vacated(&lower) {
                    // The old half of a move; the new half is an upsert.
                } else {
                    match self.deleted_file_id(&lower)? {
                        Some(id) => changes.push(Change::Remove(id)),
                        None => return Ok(None),
                    }
                }
                continue;
            }
            if !self.within_root(&lower) {
                continue;
            }
            self.remember(&entry);
            changes.push(Change::Upsert(self.item(&entry, None)?));
        }
        Ok(Some(ChangePage {
            changes,
            cursor: Cursor(cursor.to_owned()),
            has_more,
            reset,
        }))
    }

    /// The id of a deleted file this source never saw (its revision history
    /// survives the deletion). `None` for anything else, e.g. a folder.
    fn deleted_file_id(&self, path_lower: &str) -> Result<Option<ItemId>> {
        let arg = json!({"path": path_lower, "mode": "path", "limit": 1});
        match self.api.rpc("files/list_revisions", &arg) {
            Ok(value) => {
                let deleted = value
                    .get("is_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(Self::entries(&value)
                    .into_iter()
                    .next()
                    .and_then(|entry| entry.id)
                    .filter(|_| deleted)
                    .map(ItemId::new))
            }
            Err(Failure::Error(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    fn download(
        &self,
        path: &str,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
        expect_id: Option<&ItemId>,
    ) -> Result<Download> {
        let downloaded = self
            .api
            .download(&json!({ "path": path }), range.as_ref())
            .map_err(|failure| {
                if failure.has("not_file") {
                    Error::Unsupported("folders have no body")
                } else {
                    failure.into_error()
                }
            })?;
        let entry = Entry::parse(&downloaded.result).ok_or(Error::InvalidResponse("metadata"))?;
        if !entry
            .path_lower
            .as_deref()
            .is_some_and(|path| self.within_root(path))
        {
            return Err(Error::NotFound);
        }
        if let Some(id) = expect_id
            && entry.id.as_deref() != Some(id.as_str())
        {
            return Err(Error::NotFound);
        }
        let current = entry.rev.clone().map(Revision::new);
        if let Some(expected) = expect
            && current.as_ref() != Some(expected)
        {
            return Err(Error::Conflict { current });
        }
        let total = entry.size.or_else(|| {
            downloaded
                .served
                .content_range
                .as_ref()
                .and_then(|(_, t)| *t)
        });
        let body: Box<dyn Read + Send> = Box::new(downloaded.response);
        let Some(wanted) = range else {
            let len = total
                .or(downloaded.served.declared)
                .ok_or(Error::InvalidResponse("size"))?;
            let exact = http::Exact::new(body, len);
            let body: Box<dyn Read + Send> = match entry.content_hash {
                Some(hash) => Box::new(content_hash::Verifying::new(exact, hash)),
                None => Box::new(exact),
            };
            return Ok(Download {
                body,
                range: 0..len,
                total: Some(len),
                revision: current,
            });
        };
        let wanted = match total {
            Some(total) => wanted.start..wanted.end.min(total).max(wanted.start),
            None => wanted,
        };
        let download = http::ranged_body(body, Some(wanted), downloaded.served)?;
        Ok(Download {
            total,
            revision: current,
            ..download
        })
    }

    fn empty_download(&self, id: &ItemId, at: u64, expect: Option<&Revision>) -> Result<Download> {
        let entry = self.entry(id)?;
        let current = entry.rev.map(Revision::new);
        if let Some(expected) = expect
            && current.as_ref() != Some(expected)
        {
            return Err(Error::Conflict { current });
        }
        Ok(Download {
            body: Box::new(io::empty()),
            range: at..at,
            total: entry.size,
            revision: current,
        })
    }

    /// Fail with a conflict unless `id` is at `expect`.
    fn check_revision(&self, id: &ItemId, expect: &Revision) -> Result<Entry> {
        if !is_rev(expect) {
            return Err(Error::Conflict { current: None });
        }
        let entry = self.entry(id)?;
        if entry.rev.as_deref() != Some(expect.as_str()) {
            return Err(Error::Conflict {
                current: entry.rev.map(Revision::new),
            });
        }
        Ok(entry)
    }

    fn file_item(&self, value: &Value) -> Result<Item> {
        let entry = Entry::parse(value).ok_or(Error::InvalidResponse("metadata"))?;
        self.remember(&entry);
        self.item(&entry, None)
    }
}

impl CloudSource for DropboxSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            support: Support::Official,
            change_feed: ChangeFeed::Cursor,
            notify: Notify::LongPoll,
            recursive_folder_revisions: false,
            revisions: true,
            hashes: vec![HashAlgorithm::DropboxContentHash],
            share_links: true,
            quota: true,
            trash: true,
            resumable_upload: true,
            upload_chunk: self.config.upload_chunk.clamp(1, MAX_UPLOAD_REQUEST),
            range_reads: true,
            conditional_writes: true,
            multiple_parents: false,
            duplicate_names: false,
            case_insensitive: true,
            native_documents: false,
        }
    }

    fn root(&self) -> ItemId {
        ItemId::new("")
    }

    fn stat(&self, id: &ItemId) -> Result<Item> {
        if id.as_str().is_empty() {
            return Ok(Item {
                id: self.root(),
                parents: Vec::new(),
                name: self
                    .config
                    .root
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                kind: ItemKind::Folder,
                size: None,
                modified: None,
                revision: None,
                hashes: Vec::new(),
                mime: None,
            });
        }
        let entry = self.entry(id)?;
        self.item(&entry, None)
    }

    fn list(&self, folder: &ItemId) -> Result<Vec<Item>> {
        let path = self.folder_path(folder)?;
        let mut value = self
            .api
            .rpc("files/list_folder", &self.list_arg(&path, false))
            .map_err(Failure::into_error)?;
        let mut items = Vec::new();
        loop {
            for entry in Self::entries(&value) {
                if entry.kind() == Kind::Deleted || entry.id.as_deref() == Some(folder.as_str()) {
                    continue;
                }
                if folder.as_str().is_empty()
                    && entry.path_lower.as_deref() == Some(self.root_lower.as_str())
                {
                    continue;
                }
                self.remember(&entry);
                items.push(self.item(&entry, Some(folder))?);
            }
            if !value
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(items);
            }
            let cursor = value
                .get("cursor")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidResponse("cursor"))?
                .to_owned();
            value = self
                .api
                .rpc("files/list_folder/continue", &json!({ "cursor": cursor }))
                .map_err(Failure::into_error)?;
        }
    }

    fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        let (value, reset) = match cursor {
            None => (self.start_enumeration()?, true),
            Some(cursor) => match self
                .api
                .rpc("files/list_folder/continue", &json!({ "cursor": cursor.0 }))
            {
                Ok(value) => (value, false),
                Err(failure) if failure.has("reset") => (self.start_enumeration()?, true),
                Err(failure) => return Err(failure.into_error()),
            },
        };
        if let Some(page) = self.page(&value, reset)? {
            return Ok(page);
        }
        self.page(&self.start_enumeration()?, true)?
            .ok_or(Error::InvalidResponse("enumeration"))
    }

    fn wait(&self, cursor: Option<&Cursor>, timeout: Duration) -> Result<Wait> {
        let backing_off = *self.backoff_until.lock_recover();
        if let Some(until) = backing_off {
            let now = Instant::now();
            if now < until {
                return Ok(Wait::Backoff(until.saturating_duration_since(now)));
            }
        }
        let cursor = match cursor {
            Some(cursor) => cursor.0.clone(),
            None => self
                .api
                .rpc(
                    "files/list_folder/get_latest_cursor",
                    &self.list_arg(&self.config.root, true),
                )
                .map_err(Failure::into_error)?
                .get("cursor")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidResponse("cursor"))?
                .to_owned(),
        };
        let seconds = timeout
            .as_secs()
            .clamp(*LONGPOLL_SECONDS.start(), *LONGPOLL_SECONDS.end());
        let value = self
            .api
            .longpoll(&cursor, seconds)
            .map_err(Failure::into_error)?;
        let backoff = value
            .get("backoff")
            .and_then(Value::as_u64)
            .map(Duration::from_secs);
        *self.backoff_until.lock_recover() = backoff.and_then(|b| Instant::now().checked_add(b));
        let changed = value
            .get("changes")
            .and_then(Value::as_bool)
            .ok_or(Error::InvalidResponse("longpoll"))?;
        Ok(match (changed, backoff) {
            (true, _) => Wait::Changed,
            (false, Some(backoff)) => Wait::Backoff(backoff),
            (false, None) => Wait::Idle,
        })
    }

    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        if !is_dropbox_id(id) {
            return Err(if id.as_str().is_empty() {
                Error::Unsupported("folders have no body")
            } else {
                Error::NotFound
            });
        }
        if let Some(expected) = expect
            && !is_rev(expected)
        {
            return Err(Error::Conflict { current: None });
        }
        if let Some(range) = &range
            && range.start >= range.end
        {
            return self.empty_download(id, range.start, expect);
        }
        self.download(id.as_str(), range, expect, None)
    }

    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>> {
        let commit = match target {
            UploadTarget::Replace { id, expect } => {
                if !is_dropbox_id(&id) {
                    return Err(Error::NotFound);
                }
                match expect {
                    Some(expected) if !is_rev(&expected) => {
                        return Err(Error::Conflict { current: None });
                    }
                    Some(expected) => {
                        self.entry(&id)?;
                        upload::Commit::update(id.0, expected.0)
                    }
                    None => {
                        self.entry(&id)?;
                        upload::Commit::overwrite(id.0)
                    }
                }
            }
            UploadTarget::Create { parent, name } => {
                check_name(&name)?;
                let folder = self.folder_path(&parent)?;
                upload::Commit::add(format!("{folder}/{name}"))
            }
        };
        Ok(Box::new(upload::Upload::begin(self.clone(), commit, size)))
    }

    fn resume_upload(&self, state: &UploadState) -> Result<Box<dyn UploadSession>> {
        upload::Upload::resume(self.clone(), state).map(|u| Box::new(u) as Box<dyn UploadSession>)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item> {
        check_name(name)?;
        let folder = self.folder_path(parent)?;
        let value = self
            .api
            .rpc(
                "files/create_folder_v2",
                &json!({"path": format!("{folder}/{name}"), "autorename": false}),
            )
            .map_err(Failure::into_error)?;
        let mut entry = value
            .get("metadata")
            .and_then(Entry::parse)
            .ok_or(Error::InvalidResponse("metadata"))?;
        entry.tag = Some("folder".into());
        self.remember(&entry);
        self.item(&entry, Some(parent))
    }

    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item> {
        check_name(new_name)?;
        let current = match expect {
            Some(expected) => self.check_revision(id, expected)?,
            None => self.entry(id)?,
        };
        let from_lower = current
            .path_lower
            .clone()
            .ok_or(Error::InvalidResponse("path_lower"))?;
        let folder = self.folder_path(new_parent)?;
        let to = format!("{folder}/{new_name}");
        if to.to_lowercase() == from_lower && current.name == new_name {
            return self.item(&current, Some(new_parent));
        }
        let value = self
            .api
            .rpc(
                "files/move_v2",
                &json!({
                    "from_path": id.0,
                    "to_path": to,
                    "autorename": false,
                    "allow_ownership_transfer": false,
                }),
            )
            .map_err(|failure| {
                if failure.has("from_lookup") && failure.has("not_found") {
                    Error::NotFound
                } else if failure.has("conflict") {
                    Error::AlreadyExists
                } else {
                    failure.into_error()
                }
            })?;
        let entry = value
            .get("metadata")
            .and_then(Entry::parse)
            .ok_or(Error::InvalidResponse("metadata"))?;
        if let Some(to_lower) = &entry.path_lower {
            self.paths().rebase(&from_lower, to_lower);
        }
        self.remember(&entry);
        self.item(&entry, Some(new_parent))
    }

    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()> {
        if id.as_str().is_empty() {
            return Err(Error::Unsupported("deleting the mounted root"));
        }
        if !is_dropbox_id(id) {
            return Err(Error::NotFound);
        }
        let mut arg = json!({ "path": id.0 });
        if let Some(expected) = expect {
            self.check_revision(id, expected)?;
            if let Some(object) = arg.as_object_mut() {
                object.insert("parent_rev".into(), json!(expected.0));
            }
        } else {
            // Scope check: only items under the mounted root are deleted.
            self.entry(id)?;
        }
        let value = self.api.rpc("files/delete_v2", &arg).map_err(|failure| {
            if expect.is_some() && failure.has("conflict") {
                Error::Conflict { current: None }
            } else {
                failure.into_error()
            }
        })?;
        if let Some(lower) = value
            .get("metadata")
            .and_then(|m| m.get("path_lower"))
            .and_then(Value::as_str)
        {
            self.paths().remove_tree(lower);
        }
        Ok(())
    }

    fn revisions(&self, id: &ItemId) -> Result<Vec<RevisionInfo>> {
        if !is_dropbox_id(id) {
            return Err(Error::NotFound);
        }
        let value = self
            .api
            .rpc(
                "files/list_revisions",
                &json!({"path": id.0, "mode": "id", "limit": 100}),
            )
            .map_err(|failure| {
                if failure.has("not_file") {
                    Error::Unsupported("folders have no revisions")
                } else {
                    failure.into_error()
                }
            })?;
        let entries = Self::entries(&value);
        if entries.iter().any(|entry| {
            !entry
                .path_lower
                .as_deref()
                .is_some_and(|path| self.within_root(path))
        }) {
            return Err(Error::NotFound);
        }
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                Some(RevisionInfo {
                    revision: Revision::new(entry.rev?),
                    modified: entry.server_modified.as_deref().and_then(time::parse),
                    size: entry.size,
                    author: None,
                })
            })
            .collect())
    }

    fn read_revision(
        &self,
        id: &ItemId,
        revision: &Revision,
        range: Option<Range<u64>>,
    ) -> Result<Download> {
        if !is_dropbox_id(id) || !is_rev(revision) {
            return Err(Error::NotFound);
        }
        if let Some(range) = &range
            && range.start >= range.end
        {
            return Ok(Download {
                body: Box::new(io::empty()),
                range: range.start..range.start,
                total: None,
                revision: Some(revision.clone()),
            });
        }
        self.download(&format!("rev:{}", revision.0), range, None, Some(id))
    }

    fn restore_revision(&self, id: &ItemId, revision: &Revision) -> Result<Item> {
        if !is_rev(revision) {
            return Err(Error::NotFound);
        }
        let current = self.entry(id)?;
        let path = current
            .path_lower
            .ok_or(Error::InvalidResponse("path_lower"))?;
        let value = self
            .api
            .rpc("files/restore", &json!({"path": path, "rev": revision.0}))
            .map_err(|failure| {
                if failure.has("invalid_revision") {
                    Error::NotFound
                } else {
                    failure.into_error()
                }
            })?;
        self.file_item(&value)
    }

    fn share_link(&self, id: &ItemId) -> Result<String> {
        if !is_dropbox_id(id) {
            return Err(Error::NotFound);
        }
        self.entry(id)?;
        let url = |value: &Value| value.get("url").and_then(Value::as_str).map(str::to_owned);
        match self.api.rpc(
            "sharing/create_shared_link_with_settings",
            &json!({ "path": id.0 }),
        ) {
            Ok(value) => url(&value).ok_or(Error::InvalidResponse("shared link")),
            Err(failure) if failure.has("shared_link_already_exists") => {
                if let Failure::Route { error, .. } = &failure
                    && let Some(existing) = error
                        .get("shared_link_already_exists")
                        .and_then(|e| e.get("metadata"))
                        .and_then(url)
                {
                    return Ok(existing);
                }
                let value = self
                    .api
                    .rpc(
                        "sharing/list_shared_links",
                        &json!({"path": id.0, "direct_only": true}),
                    )
                    .map_err(Failure::into_error)?;
                value
                    .get("links")
                    .and_then(Value::as_array)
                    .and_then(|links| links.iter().find_map(url))
                    .ok_or(Error::InvalidResponse("shared link"))
            }
            Err(failure) => Err(failure.into_error()),
        }
    }

    fn quota(&self) -> Result<Quota> {
        let value = self
            .api
            .rpc("users/get_space_usage", &Value::Null)
            .map_err(Failure::into_error)?;
        let used = value
            .get("used")
            .and_then(Value::as_u64)
            .ok_or(Error::InvalidResponse("space usage"))?;
        let total = value
            .get("allocation")
            .and_then(|a| a.get("allocated"))
            .and_then(Value::as_u64);
        Ok(Quota { used, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_normalize_to_a_leading_slash() {
        assert_eq!(DropboxConfig::new("").root, "");
        assert_eq!(DropboxConfig::new("/").root, "");
        assert_eq!(DropboxConfig::new("Photos/2024/").root, "/Photos/2024");
    }

    #[test]
    fn only_plausible_revisions_are_sent() {
        assert!(is_rev(&Revision::new("015f9a2b3c4d")));
        assert!(!is_rev(&Revision::new("\"etag\"")));
        assert!(!is_rev(&Revision::new("abc")));
        assert!(is_dropbox_id(&ItemId::new("id:a4ayc_80_OEAAAAAAAAAXw")));
        assert!(!is_dropbox_id(&ItemId::new("missing")));
    }
}
