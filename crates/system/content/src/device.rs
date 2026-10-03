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

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use runtime_fs::blobs::{BlobClient, BlobError, MAX_BLOB_BYTES, ReadOutcome};

use crate::{
    ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored, hash_bytes,
    valid_hash, verify,
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
    /// store returned shares the one connection.
    pub fn device() -> Option<Self> {
        static DEVICE: OnceLock<Option<Arc<DeviceBackend>>> = OnceLock::new();
        let backend = DEVICE.get_or_init(|| {
            let client = BlobClient::from_environment().ok()?;
            Some(Arc::new(DeviceBackend::new(client)))
        });
        backend
            .as_ref()
            .map(|backend| Self::from_backend(Arc::clone(backend) as Arc<dyn ContentBackend>))
    }
}
