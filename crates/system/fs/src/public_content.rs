//! Public content this device keeps a copy of: small public files that
//! belong to no room and no app (Project Gutenberg's catalogue, later the
//! tiles of a map's offline areas), one copy per device, readable from every
//! workspace.
//!
//! - **What may be kept** is a fixed list of [`Source`]s, each a name, the
//!   address it is fetched from and the most it may weigh. An app names a
//!   source; it never hands the daemon an address to fetch.
//! - **The copy** is ordinary content in the content store's file area, keyed
//!   by the BLAKE3 of its bytes, with [`Retention::Fetchable`](crate::store::Retention::Fetchable): the budget
//!   evicts it like any cached content, in the same order, and only its
//!   bytes go. The entry keeps where it came from, so a read of an evicted
//!   copy fetches it again.
//! - **The name** points at the copy: a device-level pointer from the
//!   source's name to the current hash. A refresh writes the new copy whole
//!   and only then moves the pointer, so a reader sees the old copy or the
//!   new one, never half of either.
//! - **Keepers.** An app keeps a copy ([`PublicContent::keep`]): the first
//!   keeper fetches it, and only kept copies are refreshed. Releasing it
//!   removes nothing: the copy stays on the device, and the budget evicts it
//!   when it needs the room, so an app that keeps it again does not
//!   download it again.
//! - **Refresh** is conditional (`If-None-Match`, `If-Modified-Since`): the
//!   background refresh asks the origin whether the copy changed and
//!   downloads it only when it did. A copy is asked about once a
//!   [`REFRESH_EVERY`]; the background scheduler only says when the device
//!   may spend the network (os/usr/lib/systemd/user/tilo-public-content-refresh.timer).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use runtime_fs::blobs::{BlobError, BlobFuture, PublicRead, ReadOutcome};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

use crate::store::area::{AreaKey, FileArea};

/// One public file a device may keep a copy of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    /// What apps call it.
    pub name: &'static str,
    /// Where it is fetched from.
    pub url: &'static str,
    /// The most it may weigh, in bytes.
    pub max_bytes: u64,
}

/// Every public file a device may keep.
pub const SOURCES: &[Source] = &[Source {
    name: "gutenberg-catalogue",
    url: "https://www.gutenberg.org/cache/epub/feeds/pg_catalog.csv.gz",
    max_bytes: 64 * 1024 * 1024,
}];

/// How long an origin's answer about a copy holds before it is asked again.
pub const REFRESH_EVERY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The source called `name`.
#[must_use]
pub fn source(name: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.name == name)
}

/// What an origin said about a copy, to ask it next time whether it changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// What an origin answered.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// The file, and what to ask with next time.
    Changed {
        bytes: Vec<u8>,
        validator: Validator,
    },
    /// The copy asked about is still current.
    Unchanged,
}

/// Where public files are fetched from: the network, or a test's stand-in.
pub trait Origin: Send + Sync + 'static {
    /// Fetch `url`, at most `limit` bytes of it; with `since`, only if it
    /// changed since the copy that validator describes.
    fn fetch(
        &self,
        url: &'static str,
        since: Option<Validator>,
        limit: u64,
    ) -> BlobFuture<Result<Answer, String>>;
}

/// One kept copy, as the pointer file records it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Copy {
    /// The current copy's hash, once one arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(default)]
    validator: Validator,
    /// The apps that keep it.
    #[serde(default)]
    keepers: BTreeSet<String>,
    /// When its origin last answered, in seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked: Option<u64>,
}

fn epoch_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The public content this device keeps.
pub struct PublicContent {
    files: Arc<FileArea>,
    origin: Arc<dyn Origin>,
    /// Where the pointers are recorded.
    pointers: PathBuf,
    copies: std::sync::Mutex<BTreeMap<String, Copy>>,
    /// Sources being fetched right now.
    fetching: std::sync::Mutex<BTreeSet<&'static str>>,
}

fn area_key(hash: &str) -> Option<AreaKey> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(hash, &mut key).ok()?;
    Some(key)
}

fn unknown(name: &str) -> BlobError {
    BlobError::Refused(format!("no public content is called {name:?}"))
}

impl PublicContent {
    /// The public content kept in `files`, its pointers recorded in `dir`,
    /// fetched from `origin`.
    ///
    /// # Errors
    ///
    /// Returns an error when `dir` cannot be created.
    pub fn open(
        dir: &Path,
        files: Arc<FileArea>,
        origin: Arc<dyn Origin>,
    ) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        let pointers = dir.join("public-content.json");
        let copies: BTreeMap<String, Copy> = std::fs::read(&pointers)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Arc::new(Self {
            files,
            origin,
            pointers,
            copies: std::sync::Mutex::new(copies),
            fetching: std::sync::Mutex::new(BTreeSet::new()),
        }))
    }

    /// Record the pointers whole: a reader of the file after a crash finds
    /// the old pointers or the new ones.
    fn save(&self, copies: &BTreeMap<String, Copy>) {
        let Ok(bytes) = serde_json::to_vec(copies) else {
            return;
        };
        let staging = self.pointers.with_extension("json.new");
        let written = std::fs::write(&staging, bytes)
            .and_then(|()| std::fs::File::open(&staging)?.sync_all())
            .and_then(|()| std::fs::rename(&staging, &self.pointers));
        if let Err(error) = written {
            tracing::warn!(%error, "public content pointers not recorded");
        }
    }

    /// Whether the copy `hash` is wholly on this device.
    fn resident(&self, hash: Option<&str>) -> bool {
        hash.and_then(area_key)
            .and_then(|key| self.files.entry(&key))
            .is_some_and(|entry| entry.complete())
    }

    /// Keep the source `name` on this device for `app`, fetching it if it is
    /// not here.
    ///
    /// # Errors
    ///
    /// An unknown name.
    pub fn keep(self: &Arc<Self>, name: &str, app: &str) -> Result<(), BlobError> {
        let source = source(name).ok_or_else(|| unknown(name))?;
        let hash = {
            let mut copies = self.copies.lock_recover();
            let copy = copies.entry(source.name.to_owned()).or_default();
            let added = copy.keepers.insert(app.to_owned());
            let hash = copy.hash.clone();
            if added {
                self.save(&copies);
            }
            drop(copies);
            hash
        };
        if !self.resident(hash.as_deref()) {
            self.start_fetch(source);
        }
        Ok(())
    }

    /// `app` no longer keeps the source `name`. The copy stays on the device
    /// until the budget needs its room; nobody keeping it, it is no longer
    /// refreshed.
    ///
    /// # Errors
    ///
    /// An unknown name.
    pub fn release(&self, name: &str, app: &str) -> Result<(), BlobError> {
        let source = source(name).ok_or_else(|| unknown(name))?;
        let mut copies = self.copies.lock_recover();
        if copies
            .get_mut(source.name)
            .is_some_and(|copy| copy.keepers.remove(app))
        {
            self.save(&copies);
        }
        drop(copies);
        Ok(())
    }

    /// Up to `max_len` bytes of the current copy of `name` from `offset`.
    /// A copy that is not here (still arriving, or evicted) is fetched and
    /// reads as pending; one never kept on this device is unavailable.
    ///
    /// # Errors
    ///
    /// An unknown name.
    pub fn read(
        self: &Arc<Self>,
        name: &str,
        offset: u64,
        max_len: u32,
    ) -> Result<PublicRead, BlobError> {
        let source = source(name).ok_or_else(|| unknown(name))?;
        let hash = {
            let copies = self.copies.lock_recover();
            match copies.get(source.name) {
                Some(copy) if copy.hash.is_some() || !copy.keepers.is_empty() => copy.hash.clone(),
                _ => {
                    return Ok(PublicRead {
                        outcome: ReadOutcome::Unavailable,
                        hash: None,
                    });
                }
            }
        };
        let key = hash.as_deref().and_then(area_key);
        let entry = key.and_then(|key| self.files.entry(&key).map(|entry| (key, entry)));
        let Some((key, entry)) = entry.filter(|(_, entry)| entry.complete()) else {
            self.start_fetch(source);
            return Ok(PublicRead {
                outcome: ReadOutcome::Pending,
                hash,
            });
        };
        let len = u64::from(max_len).min(entry.size.saturating_sub(offset));
        let outcome = if let Ok(bytes) = self.files.read_at(&key, offset, len) {
            self.files.touch(&key);
            ReadOutcome::Ready {
                bytes,
                total_len: entry.size,
            }
        } else {
            // Evicted between the look and the read: fetched again.
            self.start_fetch(source);
            ReadOutcome::Pending
        };
        Ok(PublicRead { outcome, hash })
    }

    /// Ask the origin of every kept copy not asked about for a
    /// [`REFRESH_EVERY`] whether it changed, and take the newer one where it
    /// did. How many changed.
    pub async fn refresh(self: &Arc<Self>) -> u32 {
        self.refresh_at(SystemTime::now()).await
    }

    async fn refresh_at(self: &Arc<Self>, now: SystemTime) -> u32 {
        let due_before = epoch_seconds(now).saturating_sub(REFRESH_EVERY.as_secs());
        let due: Vec<&'static Source> = {
            let copies = self.copies.lock_recover();
            SOURCES
                .iter()
                .filter(|source| {
                    copies.get(source.name).is_some_and(|copy| {
                        !copy.keepers.is_empty()
                            && copy.checked.is_none_or(|checked| checked <= due_before)
                    })
                })
                .collect()
        };
        let mut changed = 0_u32;
        for source in due {
            if self.fetch(source, now).await {
                changed = changed.saturating_add(1);
            }
        }
        changed
    }

    /// Fetch `source` in the background, unless it is being fetched.
    fn start_fetch(self: &Arc<Self>, source: &'static Source) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let this = Arc::clone(self);
        runtime.spawn(async move {
            this.fetch(source, SystemTime::now()).await;
        });
    }

    /// Fetch `source`, conditionally when its copy is here, and make what
    /// arrived the current copy. Whether a new copy arrived.
    async fn fetch(self: &Arc<Self>, source: &'static Source, now: SystemTime) -> bool {
        if !self.fetching.lock_recover().insert(source.name) {
            return false;
        }
        let since = {
            let copies = self.copies.lock_recover();
            copies
                .get(source.name)
                .filter(|copy| self.resident(copy.hash.as_deref()))
                .map(|copy| copy.validator.clone())
        };
        let answer = self.origin.fetch(source.url, since, source.max_bytes).await;
        let arrived = match answer {
            Ok(Answer::Changed { bytes, validator }) => {
                self.install(source, &bytes, validator, now)
            }
            Ok(Answer::Unchanged) => {
                let mut copies = self.copies.lock_recover();
                if let Some(copy) = copies.get_mut(source.name) {
                    copy.checked = Some(epoch_seconds(now));
                    self.save(&copies);
                }
                drop(copies);
                false
            }
            Err(error) => {
                tracing::debug!(%error, name = source.name, "public content did not arrive");
                false
            }
        };
        self.fetching.lock_recover().remove(source.name);
        arrived
    }

    /// Make `bytes` the current copy of `source`: written whole first, the
    /// pointer moved after, the copy it replaced removed last.
    fn install(
        &self,
        source: &'static Source,
        bytes: &[u8],
        validator: Validator,
        now: SystemTime,
    ) -> bool {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let Some(key) = area_key(&hash) else {
            return false;
        };
        if let Err(error) = self.files.adopt_fetched(&key, bytes, source.url) {
            tracing::warn!(%error, name = source.name, "public content not kept");
            return false;
        }
        let previous = {
            let mut copies = self.copies.lock_recover();
            let copy = copies.entry(source.name.to_owned()).or_default();
            let previous = copy.hash.replace(hash.clone());
            copy.validator = validator;
            copy.checked = Some(epoch_seconds(now));
            self.save(&copies);
            drop(copies);
            previous
        };
        let changed = previous.as_deref() != Some(hash.as_str());
        if changed
            && let Some(old) = previous.as_deref().and_then(area_key)
            && let Err(error) = self.files.remove(&old)
        {
            tracing::debug!(%error, name = source.name, "replaced public copy not removed");
        }
        changed
    }
}

/// Public files fetched over the network.
pub struct NetOrigin {
    client: tilo_net::Client,
}

impl NetOrigin {
    /// An origin on its own connection.
    ///
    /// # Errors
    ///
    /// No network client can be made here.
    pub fn new() -> Result<Arc<Self>, String> {
        let client = tilo_net::Client::builder()
            .user_agent("Tilo/1 (+https://tilo.so)")
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self { client }))
    }
}

impl Origin for NetOrigin {
    fn fetch(
        &self,
        url: &'static str,
        since: Option<Validator>,
        limit: u64,
    ) -> BlobFuture<Result<Answer, String>> {
        use tilo_net::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};

        let mut request = self.client.get(url);
        if let Some(since) = since {
            if let Some(etag) = since.etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(modified) = since.last_modified {
                request = request.header(IF_MODIFIED_SINCE, modified);
            }
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Box::pin(async move {
            let response = request.send().await.map_err(|error| error.to_string())?;
            if response.status() == tilo_net::StatusCode::NOT_MODIFIED {
                return Ok(Answer::Unchanged);
            }
            let response = response
                .error_for_status()
                .map_err(|error| error.to_string())?;
            let text = |name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|value: &tilo_net::HeaderValue| value.to_str().ok())
                    .map(str::to_owned)
            };
            let validator = Validator {
                etag: text(ETAG),
                last_modified: text(LAST_MODIFIED),
            };
            let bytes = response
                .bytes_limited(limit)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Answer::Changed {
                bytes: bytes.to_vec(),
                validator,
            })
        })
    }
}

#[cfg(test)]
mod tests;
