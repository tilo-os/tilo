//! In-process continuation of the last committed shell selection for plain
//! route tails.
//!
//! A full shell selection re-reads all retained history. For an ordinary
//! signed route increment that lands after every tail already accepted on its
//! route, the selection's outcome is already decided by state it computed last
//! time: only that route's ordered readiness cursor advances. This cache keeps
//! exactly that cursor for a few recently written routes. The cursor is the
//! shell replay adapter's own value; the cache only holds it, opaque.
//!
//! It is valid only for the selected `CURRENT` a clean, non-batch selection
//! committed in this process, and any other retained mutation invalidates it
//! first, so a tail it cannot vouch for still takes the full selection. It
//! never retires, never verifies a pair, and never survives a restart. The
//! store invalidates it on every write whether or not a shell authority is
//! bound to the handle writing: every handle on one root shares it.
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

/// Routes whose cursors stay resident per workspace. Each holds two copies of
/// its route's selected snapshot document; routes beyond it select fully.
pub const ROUTES: usize = 8;

/// One route's cursor, as the shell replay adapter computed it.
pub type Cursor = Box<dyn Any + Send>;

#[derive(Default)]
struct TailCache {
    /// Bumped by every install and invalidation; an in-flight tail restores
    /// its cursor only if nothing else happened meanwhile.
    epoch: u64,
    /// The selected CURRENT these cursors extend; `None` is invalid.
    generation: Option<PathBuf>,
    /// Most recent first. Keys outlive invalidation so the next clean
    /// selection seeds them again.
    routes: Vec<(String, Option<Cursor>)>,
}

/// One workspace root's cache, shared by every store handle on that root so a
/// write through any of them invalidates it.
#[derive(Clone)]
pub struct SharedTailCache(Arc<Mutex<TailCache>>);

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<TailCache>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<TailCache>>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

impl SharedTailCache {
    /// The cache for an existing workspace `root`. Spellings of one directory
    /// (symlinks, `.` components) share a cache, so no handle misses a write.
    #[must_use]
    pub fn for_root(root: &Path) -> Self {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
        registry.retain(|_, cache| cache.strong_count() > 0);
        let existing = registry.get(&root).and_then(Weak::upgrade);
        let cache = existing.unwrap_or_else(|| {
            let cache = Arc::new(Mutex::new(TailCache::default()));
            registry.insert(root, Arc::downgrade(&cache));
            cache
        });
        drop(registry);
        Self(cache)
    }
    fn lock(&self) -> MutexGuard<'_, TailCache> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Drop every cursor before retained history changes outside the fast
    /// path. Returns the new epoch and the wanted route keys, most recent
    /// first.
    pub fn invalidate(&self) -> (u64, Vec<String>) {
        let mut cache = self.lock();
        cache.epoch = cache.epoch.wrapping_add(1);
        cache.generation = None;
        for (_, tail) in &mut cache.routes {
            *tail = None;
        }
        (
            cache.epoch,
            cache.routes.iter().map(|(key, _)| key.clone()).collect(),
        )
    }
    /// Publish the cursors a clean selection computed for `generation`, which
    /// it has just made the selected CURRENT. `epoch` is what the selection's
    /// own invalidation returned: if anything invalidated since, history may
    /// have changed under the selection's reads and nothing is installed.
    pub fn install(
        &self,
        epoch: u64,
        generation: PathBuf,
        wanted: &[String],
        mut seeded: BTreeMap<String, Cursor>,
    ) {
        let mut cache = self.lock();
        if cache.epoch != epoch {
            return;
        }
        cache.epoch = cache.epoch.wrapping_add(1);
        cache.generation = Some(generation);
        cache.routes = wanted
            .iter()
            .take(ROUTES)
            .map(|key| (key.clone(), seeded.remove(key)))
            .collect();
    }
    /// Whether a valid cache holds `key`'s cursor, before reading CURRENT.
    #[must_use]
    pub fn holds(&self, key: &str) -> bool {
        let cache = self.lock();
        cache.generation.is_some()
            && cache
                .routes
                .iter()
                .any(|(cached, tail)| cached == key && tail.is_some())
    }
    /// Take `key`'s cursor for one tail when it extends `current`. The cache
    /// stays invalid until [`Self::finish`] returns the cursor.
    #[must_use]
    pub fn begin(&self, current: Option<&Path>, key: &str) -> Option<(Lease, Cursor)> {
        let mut cache = self.lock();
        if current.is_none() || cache.generation.as_deref() != current {
            return None;
        }
        let tail = cache
            .routes
            .iter_mut()
            .find(|(cached, _)| cached == key)
            .and_then(|(_, tail)| tail.take())?;
        Some((
            Lease {
                epoch: cache.epoch,
                generation: cache.generation.take()?,
                key: key.to_owned(),
            },
            tail,
        ))
    }
    /// Return a cursor whose tail was suppressed or appended in full.
    pub fn finish(&self, lease: Lease, cursor: Cursor) {
        let mut cache = self.lock();
        if cache.epoch != lease.epoch || cache.generation.is_some() {
            return;
        }
        cache.generation = Some(lease.generation);
        cache.routes.retain(|(key, _)| key != &lease.key);
        cache.routes.insert(0, (lease.key, Some(cursor)));
        cache.routes.truncate(ROUTES);
    }
}

/// One route's place in the cache while its cursor is checked out.
pub struct Lease {
    epoch: u64,
    generation: PathBuf,
    key: String,
}
impl Lease {
    /// The route key the cursor belongs to.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}
