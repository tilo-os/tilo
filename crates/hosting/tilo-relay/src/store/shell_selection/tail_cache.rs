//! In-process continuation of the last committed shell selection for plain
//! route tails.
//!
//! A full [`super::Selection`] re-reads all retained history. For an ordinary
//! signed route increment that lands after every tail already accepted on its
//! route, the selection's outcome is already decided by state it computed last
//! time: the room and every route pair are unchanged (an increment is neither a
//! room frame nor a snapshot), each older input classifies exactly as before,
//! and its own route's ordered readiness cursor is the only thing it advances.
//! That outcome is `Publication::Original`, and the selected generation would
//! differ only by listing the tail, which replay re-selects before serving.
//!
//! This cache keeps exactly that cursor for a few recently written routes. It
//! is valid only for the selected CURRENT a clean, non-batch selection committed
//! in this process, and any other retained mutation invalidates it first, so a
//! tail it cannot vouch for still takes the full selection. It never retires,
//! never verifies a pair, and never survives a restart.
use super::{
    CoeditPayload, CoverageDecision, DIRECTORY, FloorSafety, Frame, OrderedTailReadiness, Path,
    PathBuf, RoomInput, StoreError, WorkspaceStore, retained,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

/// Routes whose cursors stay resident per workspace. Each holds two copies of
/// its route's selected snapshot document; routes beyond it select fully.
pub(crate) const ROUTES: usize = 8;

/// One route's cursor after every tail the committed selection accepted on it.
pub(crate) struct RouteTail {
    readiness: OrderedTailReadiness,
    /// Selection order is (author, signed sequence); a tail is a pure append
    /// only when it sorts strictly after the last accepted one.
    last: Option<([u8; 32], u64)>,
}
impl RouteTail {
    pub(crate) fn new(readiness: OrderedTailReadiness, last: Option<([u8; 32], u64)>) -> Self {
        Self { readiness, last }
    }
}

#[derive(Default)]
struct TailCache {
    /// Bumped by every install and invalidation; an in-flight tail restores
    /// its cursor only if nothing else happened meanwhile.
    epoch: u64,
    /// The selected CURRENT these cursors extend; `None` is invalid.
    generation: Option<PathBuf>,
    /// Most recent first. Keys outlive invalidation so the next clean
    /// selection seeds them again.
    routes: Vec<(String, Option<RouteTail>)>,
}

/// One workspace root's cache, shared by every store handle on that root so a
/// write through any of them invalidates it.
#[derive(Clone)]
pub(crate) struct SharedTailCache(Arc<Mutex<TailCache>>);

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<TailCache>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<TailCache>>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

impl SharedTailCache {
    /// The cache for an existing workspace `root`. Spellings of one directory
    /// (symlinks, `.` components) share a cache, so no handle misses a write.
    pub(crate) fn for_root(root: &Path) -> Self {
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
    pub(crate) fn invalidate(&self) -> (u64, Vec<String>) {
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
    pub(crate) fn install(
        &self,
        epoch: u64,
        generation: PathBuf,
        wanted: &[String],
        mut seeded: BTreeMap<String, RouteTail>,
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
    fn holds(&self, key: &str) -> bool {
        let cache = self.lock();
        cache.generation.is_some()
            && cache
                .routes
                .iter()
                .any(|(cached, tail)| cached == key && tail.is_some())
    }
    /// Take `key`'s cursor for one tail when it extends `current`. The cache
    /// stays invalid until [`Self::finish`] returns the cursor.
    fn begin(&self, current: Option<&Path>, key: &str) -> Option<FastTail> {
        let mut cache = self.lock();
        if current.is_none() || cache.generation.as_deref() != current {
            return None;
        }
        let tail = cache
            .routes
            .iter_mut()
            .find(|(cached, _)| cached == key)
            .and_then(|(_, tail)| tail.take())?;
        Some(FastTail {
            epoch: cache.epoch,
            generation: cache.generation.take()?,
            key: key.to_owned(),
            tail,
        })
    }
    /// Return a cursor whose tail was suppressed or appended in full.
    pub(crate) fn finish(&self, fast: FastTail) {
        let mut cache = self.lock();
        if cache.epoch != fast.epoch || cache.generation.is_some() {
            return;
        }
        cache.generation = Some(fast.generation);
        cache.routes.retain(|(key, _)| key != &fast.key);
        cache.routes.insert(0, (fast.key, Some(fast.tail)));
        cache.routes.truncate(ROUTES);
    }
}

/// An incoming retained tail as Check reads it: frame, route key, sequence.
fn incoming_tail(bytes: &[u8], workspace: u64) -> Option<(Frame, String, u64)> {
    let (source, CoeditPayload::RetainedDelta { channel, body }) =
        retained::payload(bytes, workspace)?
    else {
        return None;
    };
    let frame = Frame {
        source,
        channel,
        body,
        kind: RoomInput::Delta,
    };
    let (key, seq) = frame.route()?;
    Some((frame, key, seq))
}
/// The route an incoming retained tail writes, if it names one.
pub(crate) fn incoming_route(bytes: &[u8], workspace: u64) -> Option<String> {
    incoming_tail(bytes, workspace).map(|(_, key, _)| key)
}
/// Check out the cursor for the route `bytes` (a retained tail) writes, when
/// the cache extends the selected CURRENT of a bound `store`.
pub(crate) fn checkout(
    store: &WorkspaceStore,
    bytes: &[u8],
) -> Result<Option<FastTail>, StoreError> {
    if store.shell_authority().is_none() {
        return Ok(None);
    }
    let Some(key) = incoming_route(bytes, store.workspace_id) else {
        return Ok(None);
    };
    if !store.tail_cache.holds(&key) {
        return Ok(None);
    }
    let current = retained::current(&store.root.join(DIRECTORY))?;
    Ok(store.tail_cache.begin(current.as_deref(), &key))
}

/// What the selected snapshot says about an incoming tail before it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Covered by the selected snapshot: `SelectedRead::obsolete` suppresses it.
    Obsolete,
    /// Floor-safe and uncovered: write it, then try [`FastTail::append`].
    Tail,
    /// Anything else: the full selected read and selection decide.
    Select,
}

/// One route cursor checked out for a single retained tail.
pub(crate) struct FastTail {
    epoch: u64,
    generation: PathBuf,
    key: String,
    tail: RouteTail,
}
impl FastTail {
    /// The selected snapshot's view of `bytes`, as `SelectedRead::obsolete`
    /// and the selection's Check phase would decide it. For a signed route
    /// increment the other two `obsolete` tests cannot hold: the room only
    /// covers shell-tree frames and `covers_incoming_snapshot` only snapshot
    /// bodies. Anything that is not such an increment classifies `Unknown`
    /// and is left to the full read.
    pub(crate) fn admission(&self, bytes: &[u8], workspace: u64) -> Admission {
        let Some((frame, key, _)) = incoming_tail(bytes, workspace) else {
            return Admission::Select;
        };
        if key != self.key {
            return Admission::Select;
        }
        let decision = self
            .tail
            .readiness
            .classify_tail(frame.source, frame.channel, &frame.body);
        if decision.coverage == CoverageDecision::Covered {
            Admission::Obsolete
        } else if decision.floor == FloorSafety::Safe {
            Admission::Tail
        } else {
            Admission::Select
        }
    }
    /// Whether an admitted [`Admission::Tail`], now durable, is published as
    /// an original without selection: it sorts after every accepted tail on
    /// its route and is dependency-complete after them, exactly as the next
    /// selection's `ValidateExtras` would find. The cursor re-checks the floor
    /// and is unchanged when this returns false.
    pub(crate) fn append(&mut self, bytes: &[u8], workspace: u64) -> bool {
        let Some((frame, key, seq)) = incoming_tail(bytes, workspace) else {
            return false;
        };
        if key != self.key
            || self
                .tail
                .last
                .is_some_and(|last| (frame.source, seq) <= last)
            || !self
                .tail
                .readiness
                .append(frame.source, frame.channel, &frame.body)
        {
            return false;
        }
        self.tail.last = Some((frame.source, seq));
        true
    }
}
