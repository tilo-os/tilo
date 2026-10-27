//! `OneDrive` as a [`cloud_source::CloudSource`], over Microsoft Graph.
//!
//! One provider covers consumer `OneDrive`, `OneDrive` for Business and
//! `SharePoint` document libraries: all three are a Graph `drive`, addressed
//! by its id. A mount is a folder of that drive (the drive root, or any
//! folder below it).
//!
//! - **Items by id, with paths.** [`ItemId`]s are Graph item ids. Creating
//!   addresses the new item by path below its parent
//!   (`items/{parent}:/{name}:`); [`OneDriveSource::item_at`] resolves a path
//!   below the mount root, and a mount can be chosen by path
//!   ([`RootSelector::Path`]).
//! - **Change feed.** The drive's delta query is the [`Cursor`] feed: the
//!   cursor is the `@odata.nextLink`/`@odata.deltaLink` Graph returned. Graph
//!   documents delta on the drive root only, so a mount below the root
//!   follows the root's feed and keeps the items under its folder, resolving
//!   unknown parents once and caching folder membership. A `410 Gone`
//!   (`resyncRequired`) restarts the enumeration from the `Location` Graph
//!   names, as a [`ChangePage::reset`] page.
//! - **No push without a public endpoint.** Graph notifies through
//!   webhooks, which need a public HTTPS endpoint; the daemon polls the
//!   delta link instead, which costs one small request when nothing changed.
//! - **Revisions are `eTag`s.** Every write and delete sends `If-Match`;
//!   Graph answers `412` when the item moved on. File versions are listed,
//!   downloaded and restored through `versions`.
//! - **Bodies stream.** Reads fetch the item's pre-authenticated
//!   `@microsoft.graph.downloadUrl` with a `Range`; uploads go through upload
//!   sessions in multiples of 320 KiB, resumable from the session URL.
//! - **Names are case-insensitive**, and a name clash is refused
//!   (`@microsoft.graph.conflictBehavior: fail`) rather than renamed.
//! - **Deletes go to the recycle bin.**
//!
//! Shortcuts to items in other drives ("Add to my files", `remoteItem`) are
//! not part of the mount: their ids resolve only in the other drive.

#![allow(
    clippy::missing_errors_doc,
    reason = "fallible methods return cloud_source::Error, documented there"
)]

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use cloud_source::{
    Capabilities, Change, ChangeFeed, ChangePage, CloudSource, CredentialSource, Cursor, Download,
    Error, HashAlgorithm, Item, ItemId, Notify, Quota, Result, Revision, RevisionInfo, Support,
    UploadSession, UploadState, UploadTarget,
};
use reqwest::header::{CONTENT_TYPE, IF_MATCH, LOCATION, RANGE};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};

#[cfg(feature = "fake-server")]
pub mod fake;
pub mod graph;
mod http;
pub mod paths;
pub mod quickxor;
pub mod time;
mod upload;

use graph::{Drive, DriveItem, DriveType, Page, Permission, Version};
use http::{Auth, Http};
use tilo_lock::MutexRecover as _;

/// Microsoft Graph v1.0.
pub const GRAPH_API: &str = "https://graph.microsoft.com/v1.0/";
/// Upload fragments must be multiples of this (320 KiB).
pub const FRAGMENT_UNIT: u64 = 320 * 1024;
/// Default bytes per upload fragment: 10 MiB, 32 fragment units.
pub const DEFAULT_UPLOAD_CHUNK: u64 = 32 * FRAGMENT_UNIT;
/// Largest fragment: Graph takes less than 60 MiB per request.
pub const MAX_UPLOAD_CHUNK: u64 = 191 * FRAGMENT_UNIT;
/// Deepest folder chain followed when deciding whether an item is below
/// the mount root.
const MAX_DEPTH: usize = 512;

/// Which drive to mount.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveSelector {
    /// The signed-in person's own `OneDrive` (`/me/drive`).
    Me,
    /// A drive by id: another person's `OneDrive` for Business shared with
    /// them, or a `SharePoint` document library.
    Id(String),
}

/// Which folder of the drive is the mount root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootSelector {
    DriveRoot,
    Item(String),
    /// A path below the drive root, `/`-separated.
    Path(String),
}

/// What to mount. Contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneDriveConfig {
    /// The Graph endpoint, ending in `/`: [`GRAPH_API`], or a national
    /// cloud's.
    pub api: String,
    pub drive: DriveSelector,
    pub root: RootSelector,
    /// Bytes per upload fragment; rounded down to a multiple of
    /// [`FRAGMENT_UNIT`] within Graph's limits.
    pub upload_chunk: u64,
}

impl OneDriveConfig {
    /// The whole of the signed-in person's `OneDrive`.
    pub fn me() -> Self {
        Self {
            api: GRAPH_API.to_owned(),
            drive: DriveSelector::Me,
            root: RootSelector::DriveRoot,
            upload_chunk: DEFAULT_UPLOAD_CHUNK,
        }
    }

    pub fn drive(id: impl Into<String>) -> Self {
        Self {
            drive: DriveSelector::Id(id.into()),
            ..Self::me()
        }
    }
}

/// A resolved mount: the drive and folder ids a configuration named, so a
/// source can be built again without asking Graph. Contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub api: String,
    pub drive_id: String,
    pub drive_type: DriveType,
    pub drive_root: String,
    /// The id of the mounted folder (equal to `drive_root` for a whole
    /// drive).
    pub root: String,
    pub upload_chunk: u64,
}

/// A mounted `OneDrive` folder.
#[derive(Clone)]
pub struct OneDriveSource {
    mount: Arc<Mount>,
    http: Http,
    /// Folder id → whether it lies below the mount root, for mounts below
    /// the drive root.
    scope: Arc<Mutex<HashMap<String, bool>>>,
}

/// The persisted form of a delta [`Cursor`].
#[derive(Serialize, Deserialize)]
struct DeltaCursor {
    link: String,
    /// The link continues a full enumeration, which skips deleted and
    /// out-of-scope items instead of reporting them.
    #[serde(default)]
    full: bool,
}

impl OneDriveSource {
    /// Resolve `config` against Graph (the drive, its root and the mount
    /// folder) and build a source for it.
    pub fn connect(
        config: &OneDriveConfig,
        credentials: Arc<dyn CredentialSource>,
    ) -> Result<Self> {
        let http = Http::new(credentials)?;
        let api = with_slash(&config.api);
        let drive_url = match &config.drive {
            DriveSelector::Me => format!("{api}me/drive"),
            DriveSelector::Id(id) => format!("{api}drives/{}", paths::encode_segment(id)),
        };
        let drive: Drive = http.json(
            http.request(Method::GET, &format!("{drive_url}?$select=id,driveType")),
            Auth::Bearer,
        )?;
        if drive.id.is_empty() {
            return Err(Error::InvalidResponse("drive id"));
        }
        let drive_type = drive
            .drive_type
            .as_deref()
            .and_then(DriveType::parse)
            .unwrap_or(DriveType::Business);
        let base = format!("{api}drives/{}/", paths::encode_segment(&drive.id));
        let drive_root: DriveItem = http.json(
            http.request(Method::GET, &format!("{base}root?$select=id")),
            Auth::Bearer,
        )?;
        if drive_root.id.is_empty() {
            return Err(Error::InvalidResponse("drive root id"));
        }
        let root = match &config.root {
            RootSelector::DriveRoot => drive_root.id.clone(),
            RootSelector::Item(id) => folder_id(http.json(
                http.request(
                    Method::GET,
                    &format!("{base}items/{}", paths::encode_segment(id)),
                ),
                Auth::Bearer,
            )?)?,
            RootSelector::Path(path) => match paths::encode_path(path) {
                None => drive_root.id.clone(),
                Some(encoded) => folder_id(http.json(
                    http.request(Method::GET, &format!("{base}root:/{encoded}:")),
                    Auth::Bearer,
                )?)?,
            },
        };
        let mount = Mount {
            api,
            drive_id: drive.id,
            drive_type,
            drive_root: drive_root.id,
            root,
            upload_chunk: config.upload_chunk,
        };
        Ok(Self::with_http(mount, http))
    }

    /// A source for an already resolved mount. No request is made.
    pub fn new(mount: Mount, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        Ok(Self::with_http(mount, Http::new(credentials)?))
    }

    fn with_http(mut mount: Mount, http: Http) -> Self {
        mount.api = with_slash(&mount.api);
        mount.upload_chunk = fragment_size(mount.upload_chunk);
        Self {
            mount: Arc::new(mount),
            http,
            scope: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The resolved mount, for persisting.
    pub fn mount(&self) -> &Mount {
        &self.mount
    }

    /// `…/drives/{drive-id}/`.
    fn drive_url(&self) -> String {
        format!(
            "{}drives/{}/",
            self.mount.api,
            paths::encode_segment(&self.mount.drive_id)
        )
    }

    /// `…/drives/{drive-id}/items/{id}`.
    fn item_url(&self, id: &str) -> String {
        format!("{}items/{}", self.drive_url(), paths::encode_segment(id))
    }

    /// `…/items/{parent}:/{name}:`, addressing a child by name.
    fn child_url(&self, parent: &str, name: &str) -> Result<String> {
        if !paths::valid_name(name) {
            return Err(Error::InvalidResponse("file name"));
        }
        Ok(format!(
            "{}:/{}:",
            self.item_url(parent),
            paths::encode_segment(name)
        ))
    }

    fn get_item(&self, id: &str) -> Result<DriveItem> {
        self.http.json(
            self.http.request(Method::GET, &self.item_url(id)),
            Auth::Bearer,
        )
    }

    /// The item at `path` below the mount root (`/`-separated). Names match
    /// case-insensitively.
    pub fn item_at(&self, path: &str) -> Result<Item> {
        let item: DriveItem = match paths::encode_path(path) {
            None => self.get_item(&self.mount.root)?,
            Some(encoded) => self.http.json(
                self.http.request(
                    Method::GET,
                    &format!("{}:/{encoded}:", self.item_url(&self.mount.root)),
                ),
                Auth::Bearer,
            )?,
        };
        self.to_item(&item)
    }

    /// Map a Graph item; the mount root has no parents.
    fn to_item(&self, item: &DriveItem) -> Result<Item> {
        let mut out = item.to_item().ok_or(Error::InvalidResponse("drive item"))?;
        if item.id == self.mount.root {
            out.parents.clear();
        }
        Ok(out)
    }

    /// Only Graph's own origin gets the bearer token; a stored cursor or a
    /// server-provided link is checked before it is followed.
    fn check_graph_url(&self, link: &str) -> Result<()> {
        let link = Url::parse(link).map_err(|_| Error::InvalidResponse("link"))?;
        let api = Url::parse(&self.mount.api).map_err(|_| Error::InvalidResponse("api"))?;
        if link.origin() == api.origin() {
            Ok(())
        } else {
            Err(Error::InvalidResponse("link leaves the Graph endpoint"))
        }
    }

    fn lock_scope(&self) -> std::sync::MutexGuard<'_, HashMap<String, bool>> {
        self.scope.lock_recover()
    }

    /// Whether `folder` is the mount root or lies below it.
    fn inside(&self, folder: &str) -> Result<bool> {
        let (root, drive_root) = (&self.mount.root, &self.mount.drive_root);
        if root == drive_root {
            return Ok(true);
        }
        let mut chain = Vec::new();
        let mut current = folder.to_owned();
        let verdict = loop {
            if &current == root {
                break true;
            }
            if &current == drive_root || chain.len() >= MAX_DEPTH {
                break false;
            }
            let known = self.lock_scope().get(&current).copied();
            if let Some(known) = known {
                break known;
            }
            let parent = match self.get_item(&current) {
                Ok(item) => item.parent_id().map(str::to_owned),
                Err(Error::NotFound) => None,
                Err(error) => return Err(error),
            };
            chain.push(current);
            match parent {
                Some(parent) => current = parent,
                None => break false,
            }
        };
        let mut scope = self.lock_scope();
        for id in chain {
            scope.insert(id, verdict);
        }
        drop(scope);
        Ok(verdict)
    }

    /// Record a folder's membership; a folder that moved in or out of the
    /// mount invalidates what was cached about everything below it.
    fn remember_folder(&self, id: &str, inside: bool) {
        if self.mount.root == self.mount.drive_root {
            return;
        }
        let mut scope = self.lock_scope();
        if scope.insert(id.to_owned(), inside) == Some(!inside) {
            scope.clear();
        }
    }

    fn forget_folder(&self, id: &str) {
        self.lock_scope().remove(id);
    }

    fn delta_start(&self) -> String {
        format!("{}root/delta", self.drive_url())
    }

    fn delta_page(
        &self,
        link: &str,
    ) -> Result<std::result::Result<Page<DriveItem>, Option<String>>> {
        self.check_graph_url(link)?;
        let response = self
            .http
            .send_raw(self.http.request(Method::GET, link), Auth::Bearer)?;
        if response.status().is_success() {
            return http::read_json(response).map(Ok);
        }
        let failure = http::failure(response);
        if failure.status == StatusCode::GONE {
            // resyncRequired: start over, where Graph says.
            return Ok(Err(failure.location));
        }
        Err(self.http.error(failure, Auth::Bearer))
    }

    fn page_changes(&self, page: &Page<DriveItem>, full: bool) -> Result<Vec<Change>> {
        let mut changes = Vec::new();
        for entry in &page.value {
            if entry.id.is_empty()
                || entry.id == self.mount.root
                || entry.id == self.mount.drive_root
            {
                continue;
            }
            if entry.is_deleted() {
                self.forget_folder(&entry.id);
                if !full {
                    changes.push(Change::Remove(ItemId::new(entry.id.clone())));
                }
                continue;
            }
            let previous = self.lock_scope().get(&entry.id).copied();
            let inside = match entry.parent_id() {
                Some(parent) => self.inside(parent)? && !entry.is_shortcut(),
                None => false,
            };
            if entry.is_folder() || entry.package.is_some() {
                self.remember_folder(&entry.id, inside);
            } else if !full && self.mount.root != self.mount.drive_root {
                // Remember files seen outside, so the next change to them
                // elsewhere in the drive is not reported as a removal again.
                let mut scope = self.lock_scope();
                if inside {
                    scope.remove(&entry.id);
                } else {
                    scope.insert(entry.id.clone(), false);
                }
            }
            match entry.to_item() {
                Some(item) if inside => changes.push(Change::Upsert(item)),
                // Moved out of the mount (or never known to be outside):
                // gone from here.
                _ if !full && previous != Some(false) => {
                    changes.push(Change::Remove(ItemId::new(entry.id.clone())));
                }
                _ => {}
            }
        }
        Ok(changes)
    }

    /// Download `range` from a pre-authenticated URL. `size` is the item's
    /// size when known.
    fn download_from(
        &self,
        url: &str,
        range: Option<Range<u64>>,
        size: Option<u64>,
        revision: Option<Revision>,
    ) -> Result<Download> {
        let wanted = range.map(|range| match size {
            Some(size) => range.start.min(size)..range.end.min(size),
            None => range,
        });
        if let Some(wanted) = &wanted
            && wanted.start >= wanted.end
        {
            let start = wanted.start;
            return Ok(Download {
                body: Box::new(std::io::empty()),
                range: start..start,
                total: size,
                revision,
            });
        }
        let mut request = self.http.request(Method::GET, url);
        if let Some(wanted) = &wanted {
            request = request.header(
                RANGE,
                format!("bytes={}-{}", wanted.start, wanted.end.saturating_sub(1)),
            );
        }
        let response = self.http.send(request, Auth::Preauthenticated)?;
        let served = cloud_source::http::Served {
            partial: response.status() == StatusCode::PARTIAL_CONTENT,
            content_range: response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(cloud_source::http::parse_content_range),
            declared: response.content_length(),
        };
        let total = served.total().or(size);
        // A whole body is read to its declared length, else to the item's
        // size; Graph may also ignore the range and send the whole body.
        let wanted = match wanted {
            None if served.declared.is_none() => {
                Some(0..total.ok_or(Error::InvalidResponse("download length"))?)
            }
            wanted => wanted,
        };
        let download = cloud_source::http::ranged_body(Box::new(response), wanted, served)?;
        Ok(Download {
            total,
            revision,
            ..download
        })
    }

    fn if_match(
        request: reqwest::blocking::RequestBuilder,
        expect: Option<&Revision>,
    ) -> reqwest::blocking::RequestBuilder {
        match expect {
            Some(revision) => request.header(IF_MATCH, revision.as_str()),
            None => request,
        }
    }

    fn version_url(&self, id: &ItemId, revision: &Revision) -> String {
        format!(
            "{}/versions/{}",
            self.item_url(id.as_str()),
            paths::encode_segment(revision.as_str())
        )
    }
}

/// The id of a folder item, or why it cannot be a mount root.
fn folder_id(item: DriveItem) -> Result<String> {
    if item.id.is_empty() {
        return Err(Error::InvalidResponse("item id"));
    }
    if !item.is_folder() {
        return Err(Error::Unsupported("mounting a file"));
    }
    Ok(item.id)
}

fn with_slash(api: &str) -> String {
    if api.ends_with('/') {
        api.to_owned()
    } else {
        format!("{api}/")
    }
}

/// A fragment size Graph accepts: a multiple of 320 KiB below 60 MiB.
pub fn fragment_size(requested: u64) -> u64 {
    let units = (requested / FRAGMENT_UNIT).clamp(1, MAX_UPLOAD_CHUNK / FRAGMENT_UNIT);
    units.saturating_mul(FRAGMENT_UNIT)
}

impl CloudSource for OneDriveSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            support: Support::Official,
            change_feed: ChangeFeed::Cursor,
            notify: Notify::None,
            recursive_folder_revisions: false,
            revisions: true,
            hashes: vec![HashAlgorithm::QuickXor],
            share_links: true,
            quota: true,
            trash: true,
            resumable_upload: true,
            upload_chunk: self.mount.upload_chunk,
            range_reads: true,
            conditional_writes: true,
            multiple_parents: false,
            duplicate_names: false,
            case_insensitive: true,
            native_documents: true,
        }
    }

    fn root(&self) -> ItemId {
        ItemId::new(self.mount.root.clone())
    }

    fn stat(&self, id: &ItemId) -> Result<Item> {
        let item = self.get_item(id.as_str())?;
        self.to_item(&item)
    }

    fn list(&self, folder: &ItemId) -> Result<Vec<Item>> {
        let mut out = Vec::new();
        let mut link = format!("{}/children", self.item_url(folder.as_str()));
        loop {
            self.check_graph_url(&link)?;
            let page: Page<DriveItem> = self
                .http
                .json(self.http.request(Method::GET, &link), Auth::Bearer)?;
            out.extend(
                page.value
                    .iter()
                    .filter(|entry| !entry.is_shortcut() && !entry.is_deleted())
                    .filter_map(DriveItem::to_item),
            );
            match page.next_link {
                Some(next) => link = next,
                None => return Ok(out),
            }
        }
    }

    fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        let (mut link, mut full, mut reset) = match cursor {
            None => (self.delta_start(), true, true),
            Some(cursor) => {
                let stored: DeltaCursor =
                    serde_json::from_str(&cursor.0).map_err(|_| Error::CursorExpired)?;
                (stored.link, stored.full, false)
            }
        };
        let page = match self.delta_page(&link)? {
            Ok(page) => page,
            Err(location) => {
                self.lock_scope().clear();
                link = location.unwrap_or_else(|| self.delta_start());
                full = true;
                reset = true;
                self.delta_page(&link)?.map_err(|_| Error::CursorExpired)?
            }
        };
        let changes = self.page_changes(&page, full)?;
        let (next, has_more) = match (page.next_link, page.delta_link) {
            (Some(next), _) => (DeltaCursor { link: next, full }, true),
            (None, Some(delta)) => (
                DeltaCursor {
                    link: delta,
                    full: false,
                },
                false,
            ),
            (None, None) => return Err(Error::InvalidResponse("delta page without a link")),
        };
        let cursor = serde_json::to_string(&next).map_err(|_| Error::InvalidResponse("cursor"))?;
        Ok(ChangePage {
            changes,
            cursor: Cursor(cursor),
            has_more,
            reset,
        })
    }

    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        expect: Option<&Revision>,
    ) -> Result<Download> {
        let item = self.get_item(id.as_str())?;
        if item.is_folder() || item.package.is_some() {
            return Err(Error::Unsupported("reading an item without a byte body"));
        }
        let revision = item.e_tag.clone().map(Revision::new);
        if let Some(expect) = expect
            && revision.as_ref() != Some(expect)
        {
            return Err(Error::Conflict { current: revision });
        }
        let url = item
            .download_url
            .as_deref()
            .ok_or(Error::InvalidResponse("download URL"))?;
        self.download_from(url, range, Some(item.size.unwrap_or(0)), revision)
    }

    fn begin_upload(&self, target: UploadTarget, size: u64) -> Result<Box<dyn UploadSession>> {
        upload::begin(self, target, size)
    }

    fn resume_upload(&self, state: &UploadState) -> Result<Box<dyn UploadSession>> {
        upload::resume(self, state)
    }

    fn create_folder(&self, parent: &ItemId, name: &str) -> Result<Item> {
        if !paths::valid_name(name) {
            return Err(Error::InvalidResponse("folder name"));
        }
        let body = serde_json::json!({
            "name": name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail",
        });
        let item: DriveItem = self.http.json(
            self.http
                .request(
                    Method::POST,
                    &format!("{}/children", self.item_url(parent.as_str())),
                )
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
            Auth::Bearer,
        )?;
        self.to_item(&item)
    }

    fn move_item(
        &self,
        id: &ItemId,
        new_parent: &ItemId,
        new_name: &str,
        expect: Option<&Revision>,
    ) -> Result<Item> {
        if !paths::valid_name(new_name) {
            return Err(Error::InvalidResponse("file name"));
        }
        let body = serde_json::json!({
            "name": new_name,
            "parentReference": { "id": new_parent.as_str() },
        });
        let request = self
            .http
            .request(Method::PATCH, &self.item_url(id.as_str()))
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let item: DriveItem = self
            .http
            .json(Self::if_match(request, expect), Auth::Bearer)?;
        self.to_item(&item)
    }

    fn delete(&self, id: &ItemId, expect: Option<&Revision>) -> Result<()> {
        let request = self
            .http
            .request(Method::DELETE, &self.item_url(id.as_str()));
        self.http
            .send(Self::if_match(request, expect), Auth::Bearer)?;
        Ok(())
    }

    fn revisions(&self, id: &ItemId) -> Result<Vec<RevisionInfo>> {
        let mut out = Vec::new();
        let mut link = format!("{}/versions", self.item_url(id.as_str()));
        loop {
            self.check_graph_url(&link)?;
            let page: Page<Version> = self
                .http
                .json(self.http.request(Method::GET, &link), Auth::Bearer)?;
            out.extend(page.value.iter().filter_map(Version::to_info));
            match page.next_link {
                Some(next) => link = next,
                None => break,
            }
        }
        // Graph lists newest first; keep that order for equal timestamps.
        out.sort_by_key(|info| std::cmp::Reverse(info.modified));
        Ok(out)
    }

    fn read_revision(
        &self,
        id: &ItemId,
        revision: &Revision,
        range: Option<Range<u64>>,
    ) -> Result<Download> {
        let url = format!("{}/content", self.version_url(id, revision));
        let response = self
            .http
            .send_raw(self.http.request(Method::GET, &url), Auth::Bearer)?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .ok_or(Error::InvalidResponse("redirect without a location"))?;
            return self.download_from(&location, range, None, None);
        }
        if !status.is_success() {
            return Err(self.http.error(http::failure(response), Auth::Bearer));
        }
        // Served directly: the whole body, cut to the range here.
        let total = response
            .content_length()
            .ok_or(Error::InvalidResponse("download length"))?;
        let range = range.map(|r| r.start.min(total)..r.end.min(total));
        let served = cloud_source::http::Served {
            partial: false,
            content_range: None,
            declared: Some(total),
        };
        let download = cloud_source::http::ranged_body(Box::new(response), range, served)?;
        Ok(Download {
            total: Some(total),
            revision: None,
            ..download
        })
    }

    fn restore_revision(&self, id: &ItemId, revision: &Revision) -> Result<Item> {
        self.http.send(
            self.http.request(
                Method::POST,
                &format!("{}/restoreVersion", self.version_url(id, revision)),
            ),
            Auth::Bearer,
        )?;
        self.stat(id)
    }

    fn share_link(&self, id: &ItemId) -> Result<String> {
        // No scope: the organisation's default link kind applies (anyone
        // with the link on consumer OneDrive).
        let permission: Permission = self.http.json(
            self.http
                .request(
                    Method::POST,
                    &format!("{}/createLink", self.item_url(id.as_str())),
                )
                .header(CONTENT_TYPE, "application/json")
                .body(r#"{"type":"view"}"#),
            Auth::Bearer,
        )?;
        permission
            .link
            .and_then(|link| link.web_url)
            .ok_or(Error::InvalidResponse("sharing link"))
    }

    fn quota(&self) -> Result<Quota> {
        let url = format!(
            "{}drives/{}?$select=id,quota",
            self.mount.api,
            paths::encode_segment(&self.mount.drive_id)
        );
        let drive: Drive = self
            .http
            .json(self.http.request(Method::GET, &url), Auth::Bearer)?;
        drive
            .quota
            .and_then(graph::QuotaFacet::to_quota)
            .ok_or(Error::Unsupported("quota"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_are_multiples_of_320_kib_below_60_mib() {
        assert_eq!(fragment_size(0), FRAGMENT_UNIT);
        assert_eq!(fragment_size(10 * 1024 * 1024), 10 * 1024 * 1024);
        assert_eq!(fragment_size(FRAGMENT_UNIT * 3 + 5), FRAGMENT_UNIT * 3);
        assert!(fragment_size(u64::MAX) < 60 * 1024 * 1024);
        assert_eq!(fragment_size(u64::MAX) % FRAGMENT_UNIT, 0);
    }
}
