//! The device content store, through the scoped capability the launcher hands an app.
//!
//! The filesystem daemon keeps this device's content and fetches what a collaborator or another
//! of the user's devices stored; [`DeviceBackend`] is an app's view of it. Content arriving from
//! elsewhere reads as [`ContentState::Pending`] until the daemon has it. A bounded in-process
//! cache sits in front of the daemon so content drawn every frame is not read over the socket
//! every frame.
//!
//! The capability is one inherited descriptor, taken once per process: [`ContentStore::device`]
//! hands out stores over that one connection.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use runtime_fs::blobs::{BlobClient, BlobError, MAX_BLOB_BYTES, ReadOutcome};

use crate::{
    ContentBackend, ContentError, ContentReader, ContentState, ContentStore, SHARED_STORE_ENV,
    Stored, hash_bytes, sniff, valid_hash, verify,
};

/// Bytes of recently read content kept in the process.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Content larger than this is read from the daemon each time rather than cached.
const MAX_CACHED_ITEM: usize = 16 * 1024 * 1024;

/// Content in the device content store.
pub struct DeviceBackend {
    client: BlobClient,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    bytes: usize,
    entries: HashMap<String, Arc<[u8]>>,
    /// Least recently used first.
    order: VecDeque<String>,
}

impl Cache {
    fn get(&mut self, hash: &str) -> Option<Arc<[u8]>> {
        let bytes = Arc::clone(self.entries.get(hash)?);
        if let Some(at) = self.order.iter().position(|entry| entry == hash) {
            self.order.remove(at);
        }
        self.order.push_back(hash.to_owned());
        Some(bytes)
    }

    fn insert(&mut self, hash: &str, bytes: Arc<[u8]>) {
        if bytes.len() > MAX_CACHED_ITEM || self.entries.contains_key(hash) {
            return;
        }
        while self.bytes.saturating_add(bytes.len()) > CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.len());
            }
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries.insert(hash.to_owned(), bytes);
        self.order.push_back(hash.to_owned());
    }
}

fn store_error(error: BlobError) -> ContentError {
    match error {
        BlobError::TooLarge { len, limit } => ContentError::ItemTooLarge {
            actual: usize::try_from(len).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        },
        BlobError::InvalidHash => ContentError::InvalidHash(String::new()),
        other => ContentError::Io(other.to_string()),
    }
}

impl DeviceBackend {
    /// A backend over `client`, an app-scoped capability.
    pub fn new(client: BlobClient) -> Self {
        Self {
            client,
            cache: Mutex::new(Cache::default()),
        }
    }

    fn cached(&self, hash: &str) -> Option<Arc<[u8]>> {
        self.cache.lock().ok()?.get(hash)
    }

    /// Ask the daemon for `hash`, caching what it returns.
    fn fetch(&self, hash: &str) -> Result<Option<Arc<[u8]>>, ContentError> {
        match self.client.read_all(hash).map_err(store_error)? {
            ReadOutcome::Ready { bytes, .. } => {
                if !verify(hash, &bytes) {
                    return Err(ContentError::HashMismatch);
                }
                let bytes: Arc<[u8]> = bytes.into();
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(hash, Arc::clone(&bytes));
                }
                Ok(Some(bytes))
            }
            ReadOutcome::Pending | ReadOutcome::Unavailable => Ok(None),
        }
    }
}

impl ContentBackend for DeviceBackend {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_BLOB_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(ContentError::ItemTooLarge {
                actual: bytes.len(),
                limit: usize::try_from(MAX_BLOB_BYTES).unwrap_or(usize::MAX),
            });
        }
        let stored = self.client.put(&bytes, mime).map_err(store_error)?;
        if stored.hash != hash_bytes(&bytes) {
            return Err(ContentError::HashMismatch);
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(&stored.hash, bytes.into());
        }
        Ok(Stored {
            hash: stored.hash,
            len: stored.len,
        })
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        if !valid_hash(hash) {
            return Err(ContentError::InvalidHash(hash.to_owned()));
        }
        let hash = hash.to_ascii_lowercase();
        let bytes = match self.cached(&hash) {
            Some(bytes) => Some(bytes),
            None => self.fetch(&hash)?,
        };
        Ok(bytes.map(|bytes| Box::new(std::io::Cursor::new(bytes)) as ContentReader))
    }

    fn has(&self, hash: &str) -> bool {
        let hash = hash.to_ascii_lowercase();
        self.cached(&hash).is_some() || self.client.has(&hash).unwrap_or(false)
    }

    fn state(&self, hash: &str) -> ContentState {
        let hash = hash.to_ascii_lowercase();
        if self.cached(&hash).is_some() {
            return ContentState::Resolved;
        }
        match self.client.read_all(&hash) {
            Ok(ReadOutcome::Ready { bytes, .. }) if verify(&hash, &bytes) => {
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(&hash, bytes.into());
                }
                ContentState::Resolved
            }
            Ok(ReadOutcome::Pending) => ContentState::Pending,
            Ok(_) => ContentState::Unavailable,
            Err(error) => {
                tracing::warn!(%error, "device content store: read failed");
                ContentState::Unavailable
            }
        }
    }

    fn shared(&self) -> bool {
        true
    }
}

impl ContentStore {
    /// The device content store, when the launcher handed this process its capability. Every
    /// store returned shares the one connection. The first call also moves content an earlier
    /// version kept in the app's private directories into the device store, in the background.
    pub fn device() -> Option<Self> {
        static DEVICE: OnceLock<Option<Arc<DeviceBackend>>> = OnceLock::new();
        let backend = DEVICE.get_or_init(|| {
            let client = BlobClient::from_environment().ok()?;
            let backend = Arc::new(DeviceBackend::new(client));
            let store = ContentStore::from_backend(Arc::clone(&backend) as Arc<dyn ContentBackend>);
            let roots = legacy_roots(&|name| std::env::var_os(name));
            if !roots.is_empty() {
                // A process that cannot start the thread retries on its next launch.
                let _ = std::thread::Builder::new()
                    .name("content-import".into())
                    .spawn(move || import_legacy(&store, &roots));
            }
            Some(backend)
        });
        backend
            .as_ref()
            .map(|backend| Self::from_backend(Arc::clone(backend) as Arc<dyn ContentBackend>))
    }
}

/// Where apps kept content before the device store: an explicit shared directory, the private
/// directory under the user's data directory, and the older cache location.
fn legacy_roots(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(explicit) = env(SHARED_STORE_ENV).filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(explicit));
    }
    let data = env("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".local").join("share")));
    if let Some(data) = data {
        roots.push(data.join("tilo").join("content"));
    }
    let cache = env("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")));
    if let Some(cache) = cache {
        roots.push(cache.join("tilo").join("blobs"));
    }
    let mut seen = BTreeSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

fn import_legacy(store: &ContentStore, roots: &[PathBuf]) {
    for root in roots {
        match import_legacy_directory(store, root) {
            Ok(report) if !report.already_done => tracing::info!(
                root = %root.display(),
                imported = report.imported,
                skipped = report.skipped,
                "moved private content into the device content store"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                root = %root.display(), %error,
                "private content not yet moved; retrying on next launch"
            ),
        }
    }
}

/// What moving one private directory into the device store did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyImport {
    /// Items now in the device store.
    pub imported: usize,
    /// Files that were not intact content (wrong name, corrupt, oversized).
    pub skipped: usize,
    /// The directory had already been imported, or held no store.
    pub already_done: bool,
}

const LEGACY_IMPORT_MARKER: &str = ".imported-into-content-store";
const MAX_LEGACY_SCAN: usize = 65_536;

/// Copy every intact item of the sharded content directory `root` into `store`, which must be
/// shared, then mark the directory done. Idempotent; the original files are kept. A directory
/// with more than one scan's worth of entries is left unmarked, so the next launch continues.
///
/// # Errors
///
/// A store that is not shared, an I/O failure reading `root`, or the store refusing content
/// for a reason other than its size.
pub fn import_legacy_directory(
    store: &ContentStore,
    root: &Path,
) -> Result<LegacyImport, ContentError> {
    if !store.shared() {
        return Err(ContentError::Io(
            "no device content store to import into".into(),
        ));
    }
    if root.join(LEGACY_IMPORT_MARKER).exists() {
        return Ok(LegacyImport {
            already_done: true,
            ..LegacyImport::default()
        });
    }
    let shards = match std::fs::read_dir(root) {
        Ok(shards) => shards,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LegacyImport {
                already_done: true,
                ..LegacyImport::default()
            });
        }
        Err(error) => return Err(error.into()),
    };
    let mut report = LegacyImport::default();
    let mut scanned = 0_usize;
    let mut complete = true;
    'shards: for shard in shards {
        let shard = shard?;
        let name = shard.file_name();
        let Some(prefix) = name.to_str() else {
            continue;
        };
        if prefix.len() != 2
            || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !shard.file_type()?.is_dir()
        {
            continue;
        }
        for entry in std::fs::read_dir(shard.path())? {
            scanned = scanned.saturating_add(1);
            if scanned > MAX_LEGACY_SCAN {
                complete = false;
                break 'shards;
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(hash) = name.to_str().filter(|hash| valid_hash(hash)) else {
                continue;
            };
            if !hash.starts_with(prefix) || !entry.file_type()?.is_file() {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            };
            if !verify(hash, &bytes) {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
            let mut reader = bytes.as_slice();
            match store.put_reader(&mut reader, &sniff(&bytes).mime) {
                Ok(_) => report.imported = report.imported.saturating_add(1),
                Err(ContentError::ItemTooLarge { .. }) => {
                    report.skipped = report.skipped.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }
    if complete {
        std::fs::write(
            root.join(LEGACY_IMPORT_MARKER),
            b"content moved to the device content store\n",
        )?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_roots_follow_the_old_store_locations() {
        let roots = legacy_roots(&|key| match key {
            "XDG_CACHE_HOME" => Some("/run/tilo/app/.cache".into()),
            "XDG_DATA_HOME" => Some("/run/tilo/app/.local/share".into()),
            "HOME" => Some("/run/tilo/app".into()),
            _ => None,
        });
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/run/tilo/app/.local/share/tilo/content"),
                PathBuf::from("/run/tilo/app/.cache/tilo/blobs"),
            ]
        );
        let roots = legacy_roots(&|key| match key {
            "TILO_BLOB_STORE" => Some("/peer-blobs".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        });
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/peer-blobs"),
                PathBuf::from("/home/u/.local/share/tilo/content"),
                PathBuf::from("/home/u/.cache/tilo/blobs"),
            ]
        );
    }

    #[test]
    fn a_store_that_is_not_shared_receives_no_import() {
        let dir = tempfile::tempdir().unwrap();
        assert!(import_legacy_directory(&ContentStore::memory(), dir.path()).is_err());
    }
}
