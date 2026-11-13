//! The device content store, through the scoped capability the launcher hands an app.
//!
//! [`DeviceBackend`] is an app's view of the filesystem daemon's content; content
//! arriving from elsewhere reads as [`ContentState::Pending`] until the daemon has it.
//! A bounded in-process cache sits in front of the daemon, so content drawn every
//! frame is not read over the socket every frame. The capability is one inherited
//! descriptor, taken once per process.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};

use tilo_content::{
    ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored, Verifying,
    valid_hash,
};

const CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_ITEM: usize = 16 * 1024 * 1024;

/// Content in the device content store.
///
/// Nothing here holds a whole item it does not cache: a put streams to the daemon one
/// [`runtime_fs::blobs::MAX_PUT_CHUNK`] at a time, and a read that misses the cache
/// fetches ranges as it is consumed and is verified when it reaches its end.
pub struct DeviceBackend {
    client: BlobClient,
    cache: Arc<Mutex<Cache>>,
}

struct Cache {
    budget: usize,
    bytes: usize,
    /// Advances on every touch; an entry's value is its key in `order`.
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

    /// The capability is the window's (room, app) scope: the daemon serves an address
    /// only when that room references it.
    fn workspace_scoped(&self) -> bool {
        true
    }
}

/// The process's one connection to the daemon, over the descriptor the launcher
/// handed it.
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

/// The document media the session whose runtime directory this process runs in serves
/// its apps (`TILO_FS_BLOB_SOCKET`, `fs serve-media`), reached as the session it is.
///
/// A session's own process draws the same pictures its apps do but holds none of the
/// launcher's inherited descriptors ([`client`]), so it reaches the daemon by its
/// socket. The daemon serves an address through a scope only while that workspace
/// references it, so a store here reads every scope the daemon gives it with
/// [`SESSION_WORKSPACES`], the workspace in front first, and stays with the one an
/// address was read through for the addresses drawn beside it.
#[cfg(unix)]
pub(crate) fn session_store(workspace: &str) -> Option<ContentStore> {
    // One connection per socket, shared by every store a process opens over it: the driver
    // thread behind it serves many calls at once, and a picture drawn every frame is asked
    // for again each frame.
    static SESSIONS: OnceLock<Mutex<BTreeMap<std::path::PathBuf, BlobClient>>> = OnceLock::new();
    let socket = std::env::var_os(SESSION_MEDIA_SOCKET_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from(runtime_fs::blobs::DEFAULT_BLOB_SOCKET),
            std::path::PathBuf::from,
        );
    let sessions = SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let session = {
        let mut sessions = sessions.lock().ok()?;
        if let Some(session) = sessions.get(&socket) {
            session.clone()
        } else {
            let session = BlobClient::connect_session(&socket)
                .map_err(|error| tracing::debug!(%error, "no session document media"))
                .ok()?;
            sessions.insert(socket, session.clone());
            session
        }
    };
    // The workspaces this store reaches, the one drawn this pass first: the session tells
    // every workspace it draws with [`set_session_workspaces`].
    let mut workspaces: Vec<String> = vec![workspace.to_owned()];
    if let Ok(held) = SESSION_WORKSPACES.lock() {
        workspaces.extend(held.iter().filter(|held| *held != workspace).cloned());
    }
    let mut scopes = Vec::new();
    for workspace in &workspaces {
        match session.scope(workspace, SESSION_APP) {
            Ok(held) => scopes.push(held),
            // A workspace the daemon names no scope for holds none: the store still serves
            // the others.
            Err(error) => {
                tracing::debug!(%error, %workspace, "no document media scope for the session");
            }
        }
    }
    if scopes.is_empty() {
        return None;
    }
    Some(
        ContentStore::from_backend(Arc::new(SessionBackend::new(scopes)) as Arc<dyn ContentBackend>)
            .for_one_workspace(),
    )
}

/// The workspaces the session's own process draws the content of: what a store it opens
/// reads through, the workspace in front first. Set as the session starts, and again when
/// the workspaces it holds change.
#[cfg(unix)]
static SESSION_WORKSPACES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Tell every store the session's process opens from here on which workspaces it draws,
/// the one in front first.
#[cfg(unix)]
pub(crate) fn set_session_workspaces(workspaces: Vec<String>) {
    if let Ok(mut held) = SESSION_WORKSPACES.lock() {
        *held = workspaces;
    }
}

/// Content of the several workspaces a session's process draws: a read asks each scope in
/// turn, and an address one served is read through that scope from then on.
#[cfg(unix)]
pub struct SessionBackend {
    scopes: Vec<DeviceBackend>,
    cache: Arc<Mutex<Cache>>,
    held: Mutex<BTreeMap<String, usize>>,
}

#[cfg(unix)]
impl SessionBackend {
    fn new(scopes: Vec<BlobClient>) -> Self {
        Self {
            scopes: scopes.into_iter().map(DeviceBackend::new).collect(),
            cache: Arc::new(Mutex::new(Cache::new(CACHE_BYTES))),
            held: Mutex::new(BTreeMap::new()),
        }
    }

    fn cached(&self, hash: &str) -> Option<Arc<[u8]>> {
        self.cache.lock().ok()?.get(hash)
    }

    /// The scopes to ask for `hash`: the one it was last read through first, then the
    /// workspace in front's, then the rest.
    fn order(&self, hash: &str) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.scopes.len()).collect();
        if let Some(held) = self
            .held
            .lock()
            .ok()
            .and_then(|held| held.get(hash).copied())
        {
            order.retain(|of| *of != held);
            order.insert(0, held);
        }
        order
    }
}

#[cfg(unix)]
impl ContentBackend for SessionBackend {
    fn put(&self, _: &mut dyn Read, _: &str) -> Result<Stored, ContentError> {
        // The session draws content; it never stores its own: what it shows is what an
        // app's connector kept.
        Err(ContentError::Io(
            "the session's document media reads content, never stores it".to_owned(),
        ))
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        if !valid_hash(hash) {
            return Err(ContentError::InvalidHash(hash.to_owned()));
        }
        let hash = hash.to_ascii_lowercase();
        if let Some(bytes) = self.cached(&hash) {
            return Ok(Some(Box::new(std::io::Cursor::new(bytes))));
        }
        for of in self.order(&hash) {
            let Some(scope) = self.scopes.get(of) else {
                continue;
            };
            let Some(reader) = scope.open(&hash).ok().flatten() else {
                continue;
            };
            if let Ok(mut held) = self.held.lock() {
                held.insert(hash.clone(), of);
            }
            return Ok(Some(Box::new(Filling {
                inner: Verifying::new(reader, &hash),
                hash,
                kept: None,
                cache: Arc::clone(&self.cache),
            })));
        }
        Ok(None)
    }

    fn has(&self, hash: &str) -> bool {
        let hash = hash.to_ascii_lowercase();
        self.cached(&hash).is_some()
            || self
                .order(&hash)
                .into_iter()
                .any(|of| self.scopes.get(of).is_some_and(|scope| scope.has(&hash)))
    }

    fn state(&self, hash: &str) -> ContentState {
        let hash = hash.to_ascii_lowercase();
        if self.cached(&hash).is_some() {
            return ContentState::Resolved;
        }
        let mut pending = false;
        for of in self.order(&hash) {
            match self.scopes.get(of).map(|scope| scope.state(&hash)) {
                Some(ContentState::Resolved) => return ContentState::Resolved,
                Some(ContentState::Pending) => pending = true,
                _ => {}
            }
        }
        if pending {
            ContentState::Pending
        } else {
            ContentState::Unavailable
        }
    }

    fn shared(&self) -> bool {
        true
    }

    /// Every scope this store reads is one workspace's, so an address resolves only where
    /// that workspace references it.
    fn workspace_scoped(&self) -> bool {
        true
    }
}

/// The variable naming the socket a session serves its document media at
/// (`fs serve-media`), the one the launcher reads to scope every app it starts.
#[cfg(unix)]
pub(crate) const SESSION_MEDIA_SOCKET_ENV: &str = "TILO_FS_BLOB_SOCKET";

/// The app the session's own content is kept under: the reachability index is per
/// workspace, not per app, so a still one app's connector kept is read here under the
/// session's name and serves every place that shows it.
#[cfg(unix)]
const SESSION_APP: &str = "so.tilo.session";

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
