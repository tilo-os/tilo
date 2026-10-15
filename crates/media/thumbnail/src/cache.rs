//! The content-addressed thumbnail cache.
//!
//! A preview is keyed by the BLAKE3 hash of its **source** content plus the box
//! it was fitted to ([`ThumbKey`]), the same content address the rest of Tilo
//! uses end to end: a changed file gets a new key, identical content shares one
//! preview across paths and peers.
//!
//! The encoded PNGs live in a [`tilo_content::ContentStore`] — the system's one
//! content store — addressed by their *own* hash, so identical previews are
//! stored once and every read is verified. A small key index maps a
//! [`ThumbKey`] to the PNG address, and a bounded LRU sits in front of both. The store handed to [`ThumbCache::with_store`] must be dedicated to
//! previews: pruning collects everything the index no longer names.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_content::{ContentStore, DirBackend};

use crate::generate::ThumbSize;
use tilo_lock::MutexRecover as _;

/// A cache key: the source content's BLAKE3 hash plus the box the thumbnail
/// was fitted to (so the same file cached at two sizes coexists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThumbKey {
    pub content: [u8; 32],
    pub size: ThumbSize,
}

impl ThumbKey {
    /// Key a thumbnail by the BLAKE3 hash of its source bytes.
    pub fn of_bytes(bytes: &[u8], size: impl Into<ThumbSize>) -> Self {
        Self {
            content: *blake3::hash(bytes).as_bytes(),
            size: size.into(),
        }
    }

    /// Key a thumbnail by an already-known content hash (the FUSE
    /// filesystem's manifest hash, a content-store address). This is the fully
    /// content-addressed path — a thumbnail is shared across paths and peers.
    pub fn of_hash(content: [u8; 32], size: impl Into<ThumbSize>) -> Self {
        Self {
            content,
            size: size.into(),
        }
    }

    /// Key a thumbnail by a content-store address (64 hex digits). `None` for
    /// anything that is not one.
    pub fn of_address(address: &str, size: impl Into<ThumbSize>) -> Option<Self> {
        if !tilo_content::valid_hash(address) {
            return None;
        }
        let mut content = [0u8; 32];
        hex::decode_to_slice(address.to_ascii_lowercase(), &mut content).ok()?;
        Some(Self::of_hash(content, size))
    }

    /// A cheap identity key for a local file that has no precomputed content
    /// hash: BLAKE3 over `path + mtime + size`. Hashing the full bytes on the
    /// request thread would block the caller; a `(path, mtime, size)` tuple
    /// still changes when the file's content changes. Collab-FS files should
    /// use [`ThumbKey::of_hash`] for content deduplication.
    pub fn of_path_meta(path: &Path, mtime_ms: u64, len: u64, size: impl Into<ThumbSize>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(&mtime_ms.to_le_bytes());
        hasher.update(&len.to_le_bytes());
        Self {
            content: *hasher.finalize().as_bytes(),
            size: size.into(),
        }
    }

    /// The key for the frame at `at_ms` of the video this key names — one
    /// filmstrip cell. Distinct per time, still content-addressed.
    #[must_use]
    pub fn at_frame(self, at_ms: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.thumbnail.frame");
        hasher.update(&self.content);
        hasher.update(&at_ms.to_le_bytes());
        Self {
            content: *hasher.finalize().as_bytes(),
            size: self.size,
        }
    }

    /// The index entry's filename: `<hex>-<w>x<h>`.
    fn index_name(&self) -> String {
        format!(
            "{}-{}x{}",
            hex::encode(self.content),
            self.size.width,
            self.size.height
        )
    }
}

/// The in-memory tier: at most `capacity` previews, the least recently used
/// evicted first. A use counter orders them, so it needs no clock (the page
/// has no `std` one) and stays exact for the few hundred entries it holds.
#[derive(Clone)]
struct Cache {
    capacity: u64,
    entries: Arc<Mutex<Entries>>,
}

/// Previews by key, each with the use tick it was last touched at.
#[derive(Default)]
struct Entries {
    tick: u64,
    map: HashMap<ThumbKey, (u64, Arc<Vec<u8>>)>,
}

impl Entries {
    fn touch(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }
}

impl Cache {
    fn new(capacity: u64) -> Self {
        Self {
            capacity,
            entries: Arc::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock_recover()
    }

    fn get(&self, key: &ThumbKey) -> Option<Arc<Vec<u8>>> {
        let mut entries = self.lock();
        let tick = entries.touch();
        let entry = entries.map.get_mut(key)?;
        entry.0 = tick;
        let value = Arc::clone(&entry.1);
        drop(entries);
        Some(value)
    }

    fn contains_key(&self, key: &ThumbKey) -> bool {
        self.lock().map.contains_key(key)
    }

    fn insert(&self, key: ThumbKey, value: Arc<Vec<u8>>) {
        if self.capacity == 0 {
            return;
        }
        let mut entries = self.lock();
        let tick = entries.touch();
        entries.map.insert(key, (tick, value));
        while entries.map.len() as u64 > self.capacity {
            let Some(oldest) = entries
                .map
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            entries.map.remove(&oldest);
        }
        drop(entries);
    }
}

/// Content-addressed PNG-thumbnail store.
#[derive(Clone)]
pub struct ThumbCache {
    store: ContentStore,
    /// The key index directory; `None` for a cache that lives in memory only.
    index: Option<PathBuf>,
    mem: Cache,
    /// Cap on the number of indexed previews; the least-recently-written are
    /// pruned past it so the cache stays bounded across sessions.
    max_entries: usize,
}

impl ThumbCache {
    /// Open (creating if needed) a cache rooted at `dir` with default bounds:
    /// previews in a content store under `dir/content`, the key index under
    /// `dir/index`.
    ///
    /// # Errors
    ///
    /// Returns an error if `dir` can't be created.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_limits(dir, 512, 4096)
    }

    /// Open with explicit memory (`mem_capacity` entries) and index
    /// (`max_entries` previews) bounds.
    ///
    /// # Errors
    ///
    /// Returns an error if `dir` can't be created.
    pub fn with_limits(
        dir: impl AsRef<Path>,
        mem_capacity: u64,
        max_entries: usize,
    ) -> std::io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        forget_flat_layout(dir);
        Self::with_store(
            ContentStore::new(DirBackend::new(dir.join("content")).regenerable()),
            dir.join("index"),
            mem_capacity,
            max_entries,
        )
    }

    /// A cache over `store`, which must hold nothing but previews (pruning
    /// collects whatever the index at `index` no longer names).
    ///
    /// # Errors
    ///
    /// Returns an error if `index` can't be created.
    pub fn with_store(
        store: ContentStore,
        index: impl Into<PathBuf>,
        mem_capacity: u64,
        max_entries: usize,
    ) -> std::io::Result<Self> {
        let index = index.into();
        std::fs::create_dir_all(&index)?;
        Ok(Self {
            store,
            index: Some(index),
            mem: Cache::new(mem_capacity),
            max_entries,
        })
    }

    /// A cache that keeps up to `mem_capacity` previews in memory and nothing
    /// on disk: hermetic samples and tests, which must not write the user's
    /// cache.
    pub fn ephemeral(mem_capacity: u64) -> Self {
        Self {
            store: ContentStore::memory(),
            index: None,
            mem: Cache::new(mem_capacity),
            max_entries: 0,
        }
    }

    fn index_path(&self, key: &ThumbKey) -> Option<PathBuf> {
        Some(self.index.as_ref()?.join(key.index_name()))
    }

    /// The content address of `key`'s preview, if one is indexed.
    fn address(&self, key: &ThumbKey) -> Option<String> {
        let raw = std::fs::read_to_string(self.index_path(key)?).ok()?;
        let address = raw.trim();
        tilo_content::valid_hash(address).then(|| address.to_owned())
    }

    /// The cached PNG bytes for `key`: memory first, then the content store
    /// (promoting a hit into memory), else `None`. A store read is verified
    /// against its address, so a torn or tampered preview is a miss.
    pub fn get(&self, key: &ThumbKey) -> Option<Arc<Vec<u8>>> {
        if let Some(hit) = self.mem.get(key) {
            return Some(hit);
        }
        let bytes = self.store.read(&self.address(key)?)?;
        if bytes.is_empty() {
            return None;
        }
        let arc = Arc::new(bytes);
        self.mem.insert(*key, arc.clone());
        Some(arc)
    }

    /// Whether `key` is present in memory or in the store.
    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.mem.contains_key(key) || self.address(key).is_some_and(|a| self.store.has(&a))
    }

    /// Store `png` for `key`: the bytes into the content store, then the index
    /// entry naming them (temp-then-rename, so a reader never sees a
    /// half-written entry). Neither is synced: a preview can be regenerated,
    /// and a torn entry or PNG left by a crash fails validation and reads as a
    /// miss. Prunes back under the bound afterwards.
    pub fn put(&self, key: &ThumbKey, png: Vec<u8>) {
        let arc = Arc::new(png);
        self.mem.insert(*key, arc.clone());
        let Some(entry) = self.index_path(key) else {
            return;
        };
        let Ok(address) = self.store.put(arc.as_slice()) else {
            return;
        };
        if tilo_files::native::atomic::write_regenerable(&entry, address.as_bytes()).is_ok() {
            self.prune();
        }
    }

    /// Past `max_entries` indexed previews, drop the least-recently-written
    /// entries down to ~90% of the cap, then collect the previews nothing
    /// indexes any more.
    fn prune(&self) {
        let Some(Ok(rd)) = self.index.as_ref().map(std::fs::read_dir) else {
            return;
        };
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
            .filter_map(Result::ok)
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if entries.len() <= self.max_entries {
            return;
        }
        entries.sort_by_key(|(t, _)| *t);
        let target = self.max_entries.saturating_mul(9) / 10;
        let to_remove = entries.len().saturating_sub(target);
        let kept = entries.split_off(to_remove.min(entries.len()));
        for (_, path) in entries {
            let _ = std::fs::remove_file(path);
        }
        let live: BTreeSet<String> = kept
            .into_iter()
            .filter_map(|(_, path)| std::fs::read_to_string(path).ok())
            .map(|address| address.trim().to_ascii_lowercase())
            .filter(|address| tilo_content::valid_hash(address))
            .collect();
        // A store that governs its own retention reports unsupported; the
        // index is still bounded.
        let _ = self.store.collect_unreferenced(&live);
    }
}

/// Earlier caches kept `<hex>-<edge>.png` files directly in the cache
/// directory. They are unreachable under the content-store layout; remove them
/// so an upgrade does not strand the old bytes.
fn forget_flat_layout(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "png") && path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn key_is_content_addressed() {
        let hello = ThumbKey::of_bytes(b"hello", 256);
        let again = ThumbKey::of_bytes(b"hello", 256);
        assert_eq!(hello, again, "same bytes + size → same key");
        assert_ne!(
            hello,
            ThumbKey::of_bytes(b"world", 256),
            "different bytes → different key"
        );
        assert_ne!(
            hello,
            ThumbKey::of_bytes(b"hello", 128),
            "different size → different key"
        );
        assert_ne!(
            hello,
            ThumbKey::of_bytes(b"hello", ThumbSize::new(256, 144)),
            "different box → different key"
        );
        assert_ne!(
            hello.at_frame(0),
            hello.at_frame(250),
            "each frame has its own key"
        );
        assert_eq!(hello.at_frame(250), again.at_frame(250));
    }

    #[test]
    fn content_address_keys_match_hash_keys() {
        let hash = blake3::hash(b"photo");
        let by_address = ThumbKey::of_address(&hash.to_hex(), 320).unwrap();
        assert_eq!(by_address, ThumbKey::of_hash(*hash.as_bytes(), 320));
        assert!(ThumbKey::of_address("../../etc/passwd", 320).is_none());
    }

    #[test]
    fn put_then_get_hits() {
        let dir = tmpdir();
        let cache = ThumbCache::open(dir.path()).unwrap();
        let key = ThumbKey::of_bytes(b"file-content", 256);
        assert!(cache.get(&key).is_none(), "cold miss");
        cache.put(&key, vec![1, 2, 3, 4]);
        assert_eq!(
            cache.get(&key).unwrap().as_slice(),
            &[1, 2, 3, 4],
            "warm hit"
        );
        assert!(cache.contains(&key));
    }

    /// The preview bytes are in the content store, under their own address,
    /// and identical previews for two sources are stored once.
    #[test]
    fn previews_live_in_the_content_store_once() {
        let dir = tmpdir();
        let store = ContentStore::memory();
        let cache = ThumbCache::with_store(store.clone(), dir.path().join("index"), 8, 64).unwrap();
        let png = vec![7u8; 32];
        cache.put(&ThumbKey::of_bytes(b"a", 64), png.clone());
        cache.put(&ThumbKey::of_bytes(b"b", 64), png.clone());
        let address = tilo_content::hash_bytes(&png);
        assert_eq!(store.read(&address).as_deref(), Some(png.as_slice()));
        let indexed: BTreeSet<String> = std::fs::read_dir(dir.path().join("index"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert_eq!(indexed, BTreeSet::from([address]), "one stored preview");
    }

    #[test]
    fn survives_a_reopen() {
        let dir = tmpdir();
        let key = ThumbKey::of_bytes(b"persist-me", 256);
        {
            let cache = ThumbCache::open(dir.path()).unwrap();
            cache.put(&key, vec![9, 8, 7]);
        }
        // A brand-new cache instance on the same directory finds it in the
        // store — no in-memory state carried over.
        let reopened = ThumbCache::open(dir.path()).unwrap();
        assert_eq!(
            reopened.get(&key).unwrap().as_slice(),
            &[9, 8, 7],
            "thumbnail survives a restart via the content store"
        );
    }

    #[test]
    fn a_tampered_preview_is_a_miss() {
        let dir = tmpdir();
        let key = ThumbKey::of_bytes(b"src", 64);
        ThumbCache::open(dir.path())
            .unwrap()
            .put(&key, vec![1, 2, 3]);
        let address = tilo_content::hash_bytes(&[1, 2, 3]);
        let stored = dir
            .path()
            .join("content")
            .join(&address[..2])
            .join(&address);
        std::fs::write(stored, [9, 9, 9]).unwrap();
        assert!(ThumbCache::open(dir.path()).unwrap().get(&key).is_none());
    }

    #[test]
    fn cache_stays_bounded_and_collects_unindexed_previews() {
        let dir = tmpdir();
        // Cap of 10 → after inserting 40 the pruner keeps it near 90% (9).
        let cache = ThumbCache::with_limits(dir.path(), 64, 10).unwrap();
        for i in 0..40u32 {
            cache.put(
                &ThumbKey::of_bytes(format!("f{i}").as_bytes(), 256),
                i.to_le_bytes().repeat(4),
            );
        }
        let indexed = std::fs::read_dir(dir.path().join("index")).unwrap().count();
        assert!(indexed <= 10, "index pruned to its bound, got {indexed}");
        let stored: usize = std::fs::read_dir(dir.path().join("content"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|shard| std::fs::read_dir(shard.path()).unwrap().count())
            .sum();
        assert!(stored <= 10, "unindexed previews collected, got {stored}");
    }

    #[test]
    fn an_ephemeral_cache_hits_in_memory_and_writes_nothing() {
        let cache = ThumbCache::ephemeral(8);
        let key = ThumbKey::of_bytes(b"sample", 64);
        cache.put(&key, vec![4, 5, 6]);
        assert_eq!(cache.get(&key).unwrap().as_slice(), &[4, 5, 6]);
        assert!(cache.contains(&key));
    }

    #[test]
    fn the_flat_layout_is_forgotten_on_open() {
        let dir = tmpdir();
        let legacy = dir.path().join(format!("{}-256.png", "ab".repeat(32)));
        std::fs::write(&legacy, [1, 2, 3]).unwrap();
        ThumbCache::open(dir.path()).unwrap();
        assert!(!legacy.exists());
    }
}
