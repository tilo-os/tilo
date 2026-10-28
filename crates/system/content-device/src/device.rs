//! The device content store, through the scoped capability the launcher hands an app.
//!
//! The filesystem daemon keeps this device's content and fetches what a collaborator or another
//! of the user's devices stored; [`DeviceBackend`] is an app's view of it. Content arriving from
//! elsewhere reads as [`ContentState::Pending`] until the daemon has it. A bounded in-process
//! cache sits in front of the daemon so content drawn every frame is not read over the socket
//! every frame.
//!
//! The capability is one inherited descriptor, taken once per process: [`crate::device()`]
//! hands out stores over that one connection.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};

use tilo_content::{
    ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored, Verifying,
    valid_hash,
};

/// Bytes of recently read content kept in the process.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Content larger than this is read from the daemon each time rather than cached.
const MAX_CACHED_ITEM: usize = 16 * 1024 * 1024;

/// Content in the device content store.
///
/// Nothing here holds a whole item it does not cache: a put streams to the daemon one
/// [`runtime_fs::blobs::MAX_PUT_CHUNK`] at a time, and a read that misses the cache fetches
/// ranges as it is consumed and is verified when it reaches its end.
pub struct DeviceBackend {
    client: BlobClient,
    cache: Arc<Mutex<Cache>>,
}

/// Least-recently-used content, bounded by total bytes.
struct Cache {
    budget: usize,
    bytes: usize,
    /// Advances on every touch; an entry's tick is its key in `order`.
    tick: u64,
    entries: HashMap<String, (Arc<[u8]>, u64)>,
    /// Hashes by last touch, least recently used first.
    order: BTreeMap<u64, String>,
}

impl Cache {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            tick: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    fn get(&mut self, hash: &str) -> Option<Arc<[u8]>> {
        let tick = self.next_tick();
        let (bytes, touched) = self.entries.get_mut(hash)?;
        let bytes = Arc::clone(bytes);
        let key = self
            .order
            .remove(touched)
            .unwrap_or_else(|| hash.to_owned());
        *touched = tick;
        self.order.insert(tick, key);
        Some(bytes)
    }

    fn insert(&mut self, hash: &str, bytes: Arc<[u8]>) {
        if bytes.len() > MAX_CACHED_ITEM
            || bytes.len() > self.budget
            || self.entries.contains_key(hash)
        {
            return;
        }
        while self.bytes.saturating_add(bytes.len()) > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((evicted, _)) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.len());
            }
        }
        let tick = self.next_tick();
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries.insert(hash.to_owned(), (bytes, tick));
        self.order.insert(tick, hash.to_owned());
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

/// Passes a verified read through and caches a copy of it once it has ended.
struct Filling<R> {
    inner: Verifying<R>,
    hash: String,
    /// What was read so far, for content small enough to cache.
    kept: Option<Vec<u8>>,
    cache: Arc<Mutex<Cache>>,
}

impl<R: Read> Read for Filling<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = match self.inner.read(buf) {
            Ok(n) => n,
            Err(error) => {
                self.kept = None;
                return Err(error);
            }
        };
        if n == 0 {
            // `Verifying` ends only once the content matched its address.
            if let (Some(kept), Ok(mut cache)) = (self.kept.take(), self.cache.lock()) {
                cache.insert(&self.hash, kept.into());
            }
        } else if let Some(kept) = &mut self.kept {
            kept.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
        Ok(n)
    }
}

impl DeviceBackend {
    /// A backend over `client`, an app-scoped capability.
    pub fn new(client: BlobClient) -> Self {
        Self {
            client,
            cache: Arc::new(Mutex::new(Cache::new(CACHE_BYTES))),
        }
    }

    fn cached(&self, hash: &str) -> Option<Arc<[u8]>> {
        self.cache.lock().ok()?.get(hash)
    }
}

impl ContentBackend for DeviceBackend {
    /// Streams `reader` to the daemon, holding at most one chunk of it. Nothing is cached: the
    /// writer already has the bytes.
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        let stored = self.client.put_reader(reader, mime).map_err(store_error)?;
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
        if let Some(bytes) = self.cached(&hash) {
            return Ok(Some(Box::new(std::io::Cursor::new(bytes))));
        }
        let Some(reader) = self.client.open_reader(&hash).map_err(store_error)? else {
            return Ok(None);
        };
        let kept = usize::try_from(reader.len())
            .ok()
            .filter(|len| *len <= MAX_CACHED_ITEM)
            .map(Vec::with_capacity);
        Ok(Some(Box::new(Filling {
            inner: Verifying::new(reader, &hash),
            hash,
            kept,
            cache: Arc::clone(&self.cache),
        })))
    }

    fn has(&self, hash: &str) -> bool {
        let hash = hash.to_ascii_lowercase();
        self.cached(&hash).is_some() || self.client.has(&hash).unwrap_or(false)
    }

    /// Asks the daemon for an empty range: whether the content is readable, without reading it.
    fn state(&self, hash: &str) -> ContentState {
        let hash = hash.to_ascii_lowercase();
        if self.cached(&hash).is_some() {
            return ContentState::Resolved;
        }
        match self.client.read(&hash, 0, 0) {
            Ok(ReadOutcome::Ready { .. }) => ContentState::Resolved,
            Ok(ReadOutcome::Pending) => ContentState::Pending,
            Ok(ReadOutcome::Unavailable) => ContentState::Unavailable,
            Err(error) => {
                tracing::warn!(%error, "device content store: read failed");
                ContentState::Unavailable
            }
        }
    }

    fn shared(&self) -> bool {
        true
    }

    /// The capability is the window's (room, app) scope: the daemon serves an
    /// address only when that room references it.
    fn room_scoped(&self) -> bool {
        true
    }
}

/// The one launcher-granted device connection shared by stores in this process.
/// The process's one connection to the daemon, over the descriptor the launcher handed it.
pub(crate) fn client() -> Option<&'static BlobClient> {
    static CLIENT: OnceLock<Option<BlobClient>> = OnceLock::new();
    CLIENT
        .get_or_init(|| BlobClient::from_environment().ok())
        .as_ref()
}

pub(crate) fn store() -> Option<ContentStore> {
    static DEVICE: OnceLock<Option<Arc<DeviceBackend>>> = OnceLock::new();
    let backend = DEVICE.get_or_init(|| Some(Arc::new(DeviceBackend::new(client()?.clone()))));
    backend
        .as_ref()
        .map(|backend| ContentStore::from_backend(Arc::clone(backend) as Arc<dyn ContentBackend>))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(byte: u8) -> Arc<[u8]> {
        vec![byte; 4].into()
    }

    #[test]
    fn lru_get_is_constant_order_and_evicts_oldest() {
        let mut cache = Cache::new(10);
        cache.insert("a", item(1));
        cache.insert("b", item(2));
        assert_eq!(cache.get("a").as_deref(), Some([1; 4].as_slice()));
        cache.insert("c", item(3));
        assert!(
            cache.get("b").is_none(),
            "the oldest-touched entry is evicted"
        );
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
        assert_eq!(cache.bytes, 8);
        assert_eq!(cache.order.len(), cache.entries.len());

        // `a` was touched before `c`, so it goes next.
        cache.insert("d", item(4));
        assert!(cache.get("a").is_none());
        assert!(cache.get("c").is_some());
        assert!(cache.get("d").is_some());
        assert_eq!(cache.order.len(), 2);

        // Content larger than the whole budget is never cached and evicts nothing.
        cache.insert("e", Arc::from(vec![0_u8; 11]));
        assert!(cache.get("e").is_none());
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.bytes, 8);
    }
}
