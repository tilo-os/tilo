//! Which records a room holds, and which records exist at all — the two
//! facts search needs to move a record into a room in one gesture.
//!
//! A room holds **references, not copies**: bringing a record name in adds its
//! id without granting another room's document. Hydration resolves the name
//! under this room's storage and collaboration scope, so equal ids cannot
//! alias state across rooms. Copying is the separately-named
//! [`ui::record::room::duplicate`].
//!
//! The corpus has two halves: **held** is the rooms' own index documents;
//! **observed** is what apps have written, found by a background scan
//! shared by every surface so a keystroke never touches disk.
//!
//! Known gap: package state isn't partitioned per room on disk, so a
//! record is attributed to its `home` even when no room's index references it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tilo_automerge::ApplyIncoming;

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use ui::record::{RecordCheckout, RecordId, RecordKind, room};

use crate::shell::state::RoomId;

/// Package id the compositor stores its own room indexes under — not an
/// installed package, just the same directory shape so the same store
/// (atomic writes, single-writer lock) can be reused.
pub const SHELL_PACKAGE_ID: &str = "so.tilo.shell";

/// Ceiling on a record document the scan will read. Automerge can't read
/// a document in part, so anything larger is skipped — losing a search
/// row is cheaper than losing the session to memory.
const MAX_RECORD_DOC_BYTES: u64 = 16 * 1024 * 1024;

/// One record the compositor knows about, and where it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownRecord {
    pub id: RecordId,
    pub kind: RecordKind,
    /// The room the record was minted in. `None` when its own document
    /// names no home, or when only a reference is known about it.
    pub home: Option<RoomId>,
    /// Rooms whose index references this record. Sorted.
    pub held_by: Vec<RoomId>,
    /// The package whose store holds this record's document, when the
    /// record was observed there.
    pub package_id: Option<String>,
}

impl KnownRecord {
    /// Every room this record is reachable from: minted-in plus referenced-by.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut out: Vec<RoomId> = self.held_by.clone();
        if let Some(home) = self.home
            && !out.contains(&home)
        {
            out.push(home);
        }
        out.sort();
        out
    }

    /// Is this record in `room` — minted there, or referenced by it?
    pub fn is_in(&self, room: RoomId) -> bool {
        self.home == Some(room) || self.held_by.contains(&room)
    }

    /// What to call this record in a list, e.g. "annotations of book.pdf".
    pub fn title(&self) -> String {
        record_title(&self.id, &self.kind)
    }
}

/// A record found in an app's own store on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedRecord {
    pub id: RecordId,
    pub kind: RecordKind,
    pub home: Option<RoomId>,
    pub package_id: String,
}

/// One room's index document plus its writer.
struct Held {
    doc: AutoCommit,
    /// `None` for in-memory, or a room whose store couldn't be opened —
    /// runs without durability rather than refusing to reference anything.
    autosave: Option<app_state::DocWriter>,
}

#[derive(Default)]
struct ScanState {
    records: Vec<ObservedRecord>,
    /// Canonical, scan-root-confined document locations. Kept separate from
    /// public search metadata so callers cannot manufacture arbitrary paths.
    locations: BTreeMap<RecordId, ValidatedRecordLocation>,
    /// Equal record ids found in distinct scoped documents.  The legacy
    /// history loader is keyed only by id, so it must refuse this ambiguous
    /// shape rather than choosing one room's content for another.
    ambiguous: BTreeSet<RecordId>,
    /// A query before the walk finishes gets whatever is known so far
    /// rather than blocking a keystroke on the disk.
    done: bool,
    /// Incremented whenever the retained scan corpus changes.  A count is
    /// insufficient: replacing one record with another must invalidate a
    /// pagination cursor even though the cardinality did not move.
    revision: u64,
}

impl ScanState {
    fn replace(&mut self, found: ScanResult) {
        let changed = self.records != found.records
            || self.locations != found.locations
            || self.ambiguous != found.ambiguous
            || !self.done;
        if changed && self.revision == u64::MAX {
            // Keep every field, including `done`, exactly as it was. A
            // caller must never mistake a rejected replacement for a newer
            // completed scan with the same revision.
            tracing::error!(
                target: "shell::room_records",
                "record-scan revision namespace exhausted; refusing an ambiguous corpus update"
            );
            return;
        }
        self.records = found.records;
        self.locations = found.locations;
        self.ambiguous = found.ambiguous;
        self.done = true;
        if changed {
            // Checked above, before any retained search state is replaced.
            if let Some(next) = self.revision.checked_add(1) {
                self.revision = next;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidatedRecordLocation {
    document: PathBuf,
    scan_root: PathBuf,
}

#[derive(Default)]
struct ScanResult {
    records: Vec<ObservedRecord>,
    locations: BTreeMap<RecordId, ValidatedRecordLocation>,
    ambiguous: BTreeSet<RecordId>,
}

/// What [`RoomRecords::known`] last computed, and what from.
struct KnownCache {
    /// `(index generation, scanned corpus revision)`.
    key: (u64, u64),
    records: std::rc::Rc<Vec<KnownRecord>>,
}

/// The rooms' record indexes, and the records that exist.
pub struct RoomRecords {
    held: BTreeMap<RoomId, Held>,
    /// Shared with the background scan. `None` for an in-memory instance.
    scan: Option<Arc<Mutex<ScanState>>>,
    /// Records seeded directly rather than scanned.
    seeded: Vec<ObservedRecord>,
    persistent: bool,
    /// The type each record was last labelled with. Belongs to the
    /// record, not a room's reference: `RoomId` is a random uuid, so
    /// `BTreeMap` order would non-deterministically pick a winner across
    /// rooms. Last-write-wins instead; an observed on-disk kind overrides
    /// it since that's the record speaking.
    labelled: BTreeMap<RecordId, RecordKind>,
    /// Bumped by every mutation that could change [`Self::known`]'s
    /// answer, in one place ([`Self::changed`]).
    generation: u64,
    /// The [`Self::generation`] last mirrored into the workspace tree, so
    /// [`Self::replicate_with`] can no-op when nothing moved.
    replicated_generation: Option<u64>,
    known: std::cell::RefCell<Option<KnownCache>>,
}

impl RoomRecords {
    /// Monotonic revision of the retained record corpus. Search cursors use
    /// this value rather than an allocation address, which could be reused
    /// after a cache refresh (ABA) and incorrectly look current.
    pub fn search_generation(&self) -> String {
        let (generation, scan_revision) = self.revision();
        // Domain-separated deterministic projection of every input to
        // `known()`'s cache key. `generation` changes on local mutations;
        // scan_revision changes for every effective retained corpus update.
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-room-record-search-generation\0");
        hash.update(&generation.to_le_bytes());
        hash.update(&scan_revision.to_le_bytes());
        hash.finalize().to_hex().to_string()
    }
    /// An index that never touches the disk. Durability is opt-in via
    /// [`Self::attach_store`].
    pub fn in_memory() -> Self {
        Self {
            held: BTreeMap::new(),
            scan: None,
            seeded: Vec::new(),
            persistent: false,
            labelled: BTreeMap::new(),
            generation: 0,
            replicated_generation: None,
            known: std::cell::RefCell::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_scanned_records_for_test(&mut self, records: Vec<ObservedRecord>) {
        if self.scan.is_none() {
            self.scan = Some(Arc::new(Mutex::new(ScanState::default())));
        }
        if let Some(scan) = &self.scan
            && let Ok(mut scan) = scan.lock()
        {
            scan.replace(ScanResult {
                records,
                locations: BTreeMap::new(),
                ambiguous: BTreeSet::new(),
            });
        }
    }

    /// Scan the record stores under `root` now, as the background scan walks
    /// `$XDG_DATA_HOME`, and serve what it found.
    #[cfg(test)]
    pub(crate) fn scan_now_for_test(&mut self, root: &Path) {
        let mut state = ScanState::default();
        state.replace(scan_record_stores(root));
        self.scan = Some(Arc::new(Mutex::new(state)));
    }

    /// Where the scan under `root` finds package `package_id`'s record
    /// `record_dir`, as an app's sandbox writes it.
    #[cfg(test)]
    pub(crate) fn record_document_path_for_test(
        root: &Path,
        package_id: &str,
        record_dir: &str,
    ) -> PathBuf {
        root.join(launcher::appdir::ROOMS_DIR)
            .join("room")
            .join(package_id)
            .join(&*SANDBOX_DATA_SUBDIR)
            .join(package_id)
            .join(app_state::RECORDS_DIR)
            .join(record_dir)
            .join(app_state::DOCUMENT_FILE)
    }

    /// Something changed that [`Self::known`] would answer differently.
    fn can_change(&self) -> bool {
        if self.generation == u64::MAX {
            tracing::error!(
                target: "shell::room_records",
                "room-record generation namespace exhausted; refusing an ambiguous index mutation"
            );
            return false;
        }
        true
    }

    fn changed(&mut self) {
        // Every caller proves capacity with `can_change` before touching
        // retained state, so this cannot wrap or reuse a cache generation.
        if let Some(next) = self.generation.checked_add(1) {
            self.generation = next;
        }
    }

    /// Make this index durable and start looking for the records apps
    /// have written. Writes nothing — a room gains a file only when
    /// something is referenced into it.
    pub fn attach_store(&mut self) {
        if self.persistent {
            return;
        }
        if !self.can_change() {
            return;
        }
        self.persistent = true;
        self.scan = Some(shared_scan());
        self.changed();
        let rooms: Vec<RoomId> = self.held.keys().copied().collect();
        for room in rooms {
            let Some(slot) = self.held.get_mut(&room) else {
                continue;
            };
            let mut autosave = open_room_store(room, &mut slot.doc);
            if let Some(saver) = autosave.as_mut() {
                saver.request(&mut slot.doc);
            }
            slot.autosave = autosave;
        }
        self.load_indexes();
    }

    /// Add records to the known corpus without a scan.
    pub fn seed_observed(&mut self, records: impl IntoIterator<Item = ObservedRecord>) {
        let mut records = records.into_iter().peekable();
        if records.peek().is_none() || !self.can_change() {
            return;
        }
        self.seeded.extend(records);
        self.changed();
    }

    /// Bring a record into `room` by reference: the room gains the id, the
    /// document is untouched. Returns whether this added something.
    pub fn reference(&mut self, room: RoomId, id: &RecordId, kind: &RecordKind) -> bool {
        let label_changed = self.labelled.get(id) != Some(kind);
        if (self.holds(room, id) && !label_changed) || !self.can_change() {
            return false;
        }
        let slot = self.index_mut(room);
        let added = room::reference(&mut slot.doc, id, kind);
        if added && let Some(saver) = slot.autosave.as_mut() {
            saver.request(&mut slot.doc);
        }
        self.labelled.insert(id.clone(), kind.clone());
        self.changed();
        added
    }

    /// Drop a room's reference to a record. Stops future changes flowing
    /// to the room's members but cannot un-share bytes a peer already synced.
    pub fn unreference(&mut self, room: RoomId, id: &RecordId) -> bool {
        if !self.holds(room, id) || !self.can_change() {
            return false;
        }
        let Some(slot) = self.held.get_mut(&room) else {
            return false;
        };
        let dropped = room::unreference(&mut slot.doc, id);
        if dropped && let Some(saver) = slot.autosave.as_mut() {
            saver.request(&mut slot.doc);
        }
        self.changed();
        dropped
    }

    /// Does `room`'s index reference `id`?
    pub fn holds(&self, room: RoomId, id: &RecordId) -> bool {
        self.held
            .get(&room)
            .is_some_and(|h| room::holds(&h.doc, id))
    }

    /// Every record `room`'s index references.
    pub fn records_in(&self, room: RoomId) -> Vec<RecordCheckout> {
        self.held
            .get(&room)
            .map(|h| room::references(&h.doc))
            .unwrap_or_default()
    }

    /// Every record this compositor knows about, merged from the rooms'
    /// indexes and the records apps have written, sorted by id.
    ///
    /// **Runs on every keystroke**, so after the first call it does no
    /// CRDT reads: walking a room's index measured 19ms over 300 records.
    /// Cached, keyed on generation plus scan progress.
    pub fn known(&self) -> std::rc::Rc<Vec<KnownRecord>> {
        let scan_revision = self
            .scan
            .as_ref()
            .and_then(|s| s.lock().ok().map(|st| st.revision))
            .unwrap_or(0);
        let key = (self.generation, scan_revision);
        if let Some(cached) = self.known.borrow().as_ref()
            && cached.key == key
        {
            return std::rc::Rc::clone(&cached.records);
        }
        let records = std::rc::Rc::new(self.compute_known());
        *self.known.borrow_mut() = Some(KnownCache {
            key,
            records: std::rc::Rc::clone(&records),
        });
        records
    }

    fn compute_known(&self) -> Vec<KnownRecord> {
        // A record id is not globally authoritative.  Key the retained
        // search corpus by `(room scope, id)` so an equal id observed in two
        // rooms becomes two rows rather than silently borrowing one room's
        // metadata and location.
        let mut by_id: BTreeMap<(Option<RoomId>, RecordId), KnownRecord> = BTreeMap::new();

        let mut observe = |o: &ObservedRecord| {
            let entry = by_id
                .entry((o.home, o.id.clone()))
                .or_insert_with(|| KnownRecord {
                    id: o.id.clone(),
                    kind: o.kind.clone(),
                    home: None,
                    held_by: Vec::new(),
                    package_id: None,
                });
            if entry.home.is_none() {
                entry.home = o.home;
            }
            if entry.package_id.is_none() {
                entry.package_id = Some(o.package_id.clone());
            }
            if entry.kind.as_str().is_empty() {
                entry.kind = o.kind.clone();
            }
        };
        if let Some(scan) = self.scan.as_ref()
            && let Ok(state) = scan.lock()
        {
            state.records.iter().for_each(&mut observe);
        }
        self.seeded.iter().for_each(&mut observe);

        for (room, held) in &self.held {
            for checkout in room::references(&held.doc) {
                let entry = by_id
                    .entry((Some(*room), checkout.id.clone()))
                    .or_insert_with(|| KnownRecord {
                        id: checkout.id.clone(),
                        kind: RecordKind::new(""),
                        home: Some(*room),
                        held_by: Vec::new(),
                        package_id: None,
                    });
                if !entry.held_by.contains(room) {
                    entry.held_by.push(*room);
                }
            }
        }
        for ((_, id), r) in &mut by_id {
            if r.kind.as_str().is_empty()
                && let Some(k) = self.labelled.get(id)
            {
                r.kind = k.clone();
            }
            r.held_by.sort();
        }
        by_id.into_values().collect()
    }

    /// Whether the background scan has finished; nothing waits on it.
    pub fn scan_complete(&self) -> bool {
        self.scan
            .as_ref()
            .is_none_or(|s| s.lock().is_ok_and(|st| st.done))
    }

    /// The scanned record documents, as a handle another thread reads
    /// through: [`LocalRecordReader::load`] loads one without opening a
    /// [`ui::record::RecordStore`], and [`LocalRecordReader::revision`]
    /// witnesses what it would read. Both touch the disk, which the
    /// compositor loop must not wait for. `None` for an in-memory instance.
    pub(crate) fn local_reader(&self) -> Option<LocalRecordReader> {
        self.scan.clone().map(|scan| LocalRecordReader { scan })
    }

    /// Changes whenever the room indexes or the scanned corpus change, so a
    /// frame can tell cheaply that nothing about the rooms' records moved.
    pub fn revision(&self) -> (u64, u64) {
        let scan_revision = self
            .scan
            .as_ref()
            .and_then(|scan| scan.lock().ok().map(|state| state.revision))
            .unwrap_or(0);
        (self.generation, scan_revision)
    }

    /// Write out every pending index and wait for it to land.
    pub fn flush(&mut self) {
        for slot in self.held.values_mut() {
            if let Some(saver) = slot.autosave.as_mut() {
                saver.request(&mut slot.doc);
                saver.flush();
            }
        }
    }

    /// Exchange this machine's room indexes with the peer-replication
    /// path, returning whether anything arrived from a peer.
    ///
    /// The shell does this rather than the tree: a room's index sits
    /// behind `Rc<RefCell<_>>` on the overlay host while replication
    /// holds the tree across `Arc<RwLock<_>>` on a `Send` boundary —
    /// neither can hold the other, so the frame pump reconciles them
    /// here. Only references cross, never who has a record open.
    pub fn replicate_with(&mut self, tree: &mut crate::shell::state::WorkspaceTree) -> bool {
        if self.replicated_generation != Some(self.generation) {
            self.replicated_generation = Some(self.generation);
            let rooms: Vec<RoomId> = self.held.keys().copied().collect();
            for room in rooms {
                let held: BTreeMap<String, String> = self
                    .records_in(room)
                    .into_iter()
                    .map(|c| (c.id.as_str().to_string(), c.kind.as_str().to_string()))
                    .collect();
                tree.set_room_records(room, held);
            }
        }

        let inbound = tree.take_inbound_room_records();
        if inbound.is_empty() {
            return false;
        }
        for (room, id, kind) in inbound {
            let Some(record) = RecordId::parse(&id) else {
                tracing::debug!(
                    target: "shell::room_records",
                    room = %room,
                    record = %id,
                    "a peer referenced a record id this build cannot parse; ignored",
                );
                continue;
            };
            self.reference(room, &record, &RecordKind::new(kind));
        }
        // Re-arm so a guest that just learned of a record can source it
        // to the next peer, letting a room converge when not all connected.
        self.replicated_generation = None;
        true
    }

    /// The room's index, opening (and creating) its store on first write.
    fn index_mut(&mut self, room: RoomId) -> &mut Held {
        let persistent = self.persistent;
        self.held.entry(room).or_insert_with(|| {
            let mut doc = AutoCommit::new();
            let autosave = if persistent {
                open_room_store(room, &mut doc)
            } else {
                None
            };
            Held { doc, autosave }
        })
    }

    /// Load every room index already on disk.
    fn load_indexes(&mut self) {
        let dir = rooms_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(room) = RoomId::parse(&name) else {
                continue;
            };
            let _ = self.index_mut(room);
            for checkout in self.records_in(room) {
                self.labelled.entry(checkout.id).or_insert(checkout.kind);
            }
        }
    }
}

impl Default for RoomRecords {
    fn default() -> Self {
        Self::in_memory()
    }
}

/// Open (and merge in) the stored index for `room`, returning its writer.
fn open_room_store(room: RoomId, doc: &mut AutoCommit) -> Option<app_state::DocWriter> {
    let mut store = match app_state::DocStore::open_room(SHELL_PACKAGE_ID, &room.to_string()) {
        Ok(store) => store,
        Err(e) => {
            tracing::error!(
                room = %room,
                error = %e,
                "workspace index unavailable; this workspace's items are not persisted"
            );
            return None;
        }
    };
    let stored = match store.load() {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(
                room = %room,
                error = %e,
                "stored room index could not be read; continuing with what is live"
            );
            None
        }
    };
    let mut autosave = store.writer();
    if let Some(mut stored) = stored {
        autosave.seed(&mut stored);
        if let Err(e) = doc.merge_incoming(&mut stored) {
            tracing::error!(room = %room, error = %e, "stored room index could not be merged");
        }
    }
    Some(autosave)
}

fn rooms_dir() -> PathBuf {
    cli::standalone_state_dir(SHELL_PACKAGE_ID).join(app_state::ROOMS_DIR)
}

/// The process-wide record scan, started on first use — one walk per
/// process rather than one per palette host (tests build many).
fn shared_scan() -> Arc<Mutex<ScanState>> {
    static SCAN: OnceLock<Arc<Mutex<ScanState>>> = OnceLock::new();
    SCAN.get_or_init(|| {
        let state = Arc::new(Mutex::new(ScanState::default()));
        let handle = Arc::clone(&state);
        let root = cli::data_home();
        std::thread::Builder::new()
            .name("tilo-record-scan".into())
            .spawn(move || {
                let found = scan_record_stores(&root);
                if let Ok(mut st) = handle.lock() {
                    st.replace(found);
                }
            })
            .ok();
        state
    })
    .clone()
}

/// Walk every room's packages and read each record's identity. The
/// compositor is the only component that reads across rooms (Super+K
/// shows records from other rooms, which a sandboxed app cannot see).
/// Deliberately shallow, bounded by record count, not home size.
fn scan_record_stores(root: &Path) -> ScanResult {
    let mut out = ScanResult::default();
    let Ok(scan_root) = root.canonicalize() else {
        return out;
    };
    let Ok(rooms) = std::fs::read_dir(root.join(launcher::appdir::ROOMS_DIR)) else {
        return out;
    };
    for room in rooms.flatten() {
        let Ok(packages) = std::fs::read_dir(room.path()) else {
            continue;
        };
        for package in packages.flatten() {
            let Some(package_id) = package.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if package_id == SHELL_PACKAGE_ID {
                continue;
            }
            let state = package.path().join(&*SANDBOX_DATA_SUBDIR).join(&package_id);
            // Keep the legacy root readable during migration, then walk the
            // explicit room scopes.  The latter is bounded by room/record
            // directories, never by arbitrary app data.
            let mut record_roots = vec![state.join(app_state::RECORDS_DIR)];
            if let Ok(rooms) = std::fs::read_dir(state.join(app_state::ROOMS_DIR)) {
                record_roots.extend(
                    rooms
                        .flatten()
                        .map(|room| room.path().join(app_state::RECORDS_DIR)),
                );
            }
            for records in record_roots {
                let Ok(entries) = std::fs::read_dir(&records) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Some(location) = validate_record_location(
                        &scan_root,
                        &entry.path().join(app_state::DOCUMENT_FILE),
                    ) else {
                        continue;
                    };
                    let Some(observed) = read_record_header(&location.document, &package_id) else {
                        continue;
                    };
                    match out.locations.get(&observed.id) {
                        Some(current) if current.document != location.document => {
                            out.ambiguous.insert(observed.id.clone());
                        }
                        Some(_) => {}
                        None => {
                            out.locations.insert(observed.id.clone(), location);
                        }
                    }
                    out.records.push(observed);
                }
            }
        }
    }
    out.records.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    out
}

/// The scanned record documents, read from any thread: see
/// [`RoomRecords::local_reader`].
#[derive(Clone)]
pub(crate) struct LocalRecordReader {
    scan: Arc<Mutex<ScanState>>,
}

impl LocalRecordReader {
    /// The scanned location of `id`, unless two rooms' documents claim it;
    /// `hydrating` says a refusal is worth a warning.
    fn location(&self, id: &RecordId, hydrating: bool) -> Option<ValidatedRecordLocation> {
        let scan = self.scan.lock().ok()?;
        if scan.ambiguous.contains(id) {
            if hydrating {
                tracing::warn!(record = %id, "refusing ambiguous cross-room record hydration");
            }
            return None;
        }
        scan.locations.get(id).cloned()
    }

    /// Load scanned record `id` without opening a
    /// [`ui::record::RecordStore`], creating directories, touching actor
    /// state, or acquiring a writer lock. The canonical path and size bound
    /// are revalidated on every read so a stale or replaced scan result
    /// fails closed. Reads the disk: never on the compositor loop.
    pub(crate) fn load(&self, id: &RecordId) -> Option<AutoCommit> {
        read_validated_record_document(id, &self.location(id, true)?)
    }

    /// A witness of the bytes [`Self::load`] would read for `id` now: the
    /// document's length and modification time, from one `stat`. `None`
    /// when the record has no scanned location.
    pub(crate) fn revision(&self, id: &RecordId) -> Option<u64> {
        let location = self.location(id, false)?;
        let metadata = std::fs::metadata(&location.document).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-room-record-revision\0");
        hash.update(location.document.as_os_str().as_encoded_bytes());
        hash.update(&metadata.len().to_le_bytes());
        hash.update(&modified.as_nanos().to_le_bytes());
        let mut word = [0_u8; 8];
        word.copy_from_slice(&hash.finalize().as_bytes()[..8]);
        Some(u64::from_le_bytes(word))
    }
}

fn validate_record_location(scan_root: &Path, document: &Path) -> Option<ValidatedRecordLocation> {
    let document = document.canonicalize().ok()?;
    if !document.starts_with(scan_root) {
        tracing::warn!(
            path = %document.display(),
            root = %scan_root.display(),
            "record document escaped the bounded scan root"
        );
        return None;
    }
    let metadata = std::fs::metadata(&document).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_DOC_BYTES {
        tracing::debug!(
            path = %document.display(),
            bytes = metadata.len(),
            "record document is not a bounded regular file"
        );
        return None;
    }
    Some(ValidatedRecordLocation {
        document,
        scan_root: scan_root.to_path_buf(),
    })
}

fn read_validated_record_document(
    expected: &RecordId,
    location: &ValidatedRecordLocation,
) -> Option<AutoCommit> {
    let current = validate_record_location(&location.scan_root, &location.document)?;
    let bytes = std::fs::read(&current.document).ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_RECORD_DOC_BYTES {
        return None;
    }
    let doc = AutoCommit::load_with_options(
        &bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
    )
    .ok()?;
    (ui::record::identity(&doc)?.id == *expected).then_some(doc)
}

/// The path `XDG_DATA_HOME` takes inside an app's directory, derived from
/// the launcher's own announcement so the walk cannot drift from it.
static SANDBOX_DATA_SUBDIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
    let root = Path::new(launcher::appdir::SANDBOX_APP_DIR);
    launcher::appdir::sandbox_env()
        .into_iter()
        .find(|(k, _)| k == "XDG_DATA_HOME")
        .and_then(|(_, v)| Path::new(&v).strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
});

/// Read one record document's identity, type and home room.
fn read_record_header(path: &Path, package_id: &str) -> Option<ObservedRecord> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_RECORD_DOC_BYTES {
        return None;
    }
    let doc = AutoCommit::load_with_options(
        &bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
    )
    .ok()?;
    let checkout = ui::record::identity(&doc)?;
    let home = ui::record::home(&doc).and_then(|h| RoomId::parse(&h));
    Some(ObservedRecord {
        id: checkout.id,
        kind: checkout.kind,
        home,
        package_id: package_id.to_string(),
    })
}

/// What to call a record in a list. See [`KnownRecord::title`].
pub fn record_title(id: &RecordId, kind: &RecordKind) -> String {
    if let Some(path) = id.file_path() {
        return leaf_of(&path);
    }
    if let Some(rest) = id.as_str().strip_prefix("derived:")
        && let Some((role, target)) = rest.split_once('@')
    {
        let target_name = RecordId::parse(target).map_or_else(
            || target.to_string(),
            |t| record_title(&t, &RecordKind::new("")),
        );
        return format!("{role} of {target_name}");
    }
    if !kind.as_str().is_empty() {
        return kind.as_str().to_string();
    }
    id.as_str().to_string()
}

fn leaf_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

/// Human name for the room a record lives in — a raw uuid in a palette row
/// tells the reader nothing and looks like a bug.
pub fn room_label(tree: &crate::shell::state::WorkspaceTree, room: RoomId) -> Option<String> {
    tree.desktop
        .workspaces
        .iter()
        .find(|w| w.room == room)
        .map(|w| w.name.clone())
}

#[cfg(test)]
mod tests {
    use automerge::transaction::{CommitOptions, Transactable};

    use super::*;

    fn room() -> RoomId {
        RoomId::new()
    }

    #[test]
    fn rooms_hold_distinct_recognisable_references_to_untouched_records() {
        // Phase `a_room_holds_references_and_the_record_is_untouched`.
        {
            let mut rr = RoomRecords::in_memory();
            let a = room();
            let b = room();
            let id = RecordId::minted();
            let kind = RecordKind::new("book");

            assert!(rr.reference(a, &id, &kind), "first reference adds");
            assert!(
                !rr.reference(a, &id, &kind),
                "referencing twice adds nothing"
            );
            assert!(rr.holds(a, &id));
            assert!(!rr.holds(b, &id));

            assert!(rr.reference(b, &id, &kind));
            assert!(rr.holds(a, &id) && rr.holds(b, &id));
            assert_eq!(rr.records_in(a), rr.records_in(b));

            assert!(rr.unreference(a, &id));
            assert!(!rr.holds(a, &id));
            assert!(rr.holds(b, &id), "the record is still in the other room");
        }

        // Phase `known_keeps_equal_record_ids_in_different_rooms_distinct`.
        {
            let mut rr = RoomRecords::in_memory();
            let home = room();
            let other = room();
            let scanned = RecordId::for_file("/work/report.odt");
            rr.seed_observed([ObservedRecord {
                id: scanned.clone(),
                kind: RecordKind::new("document"),
                home: Some(home),
                package_id: "so.tilo.word-processor".into(),
            }]);
            rr.reference(other, &scanned, &RecordKind::new("document"));

            let known = rr.known();
            assert_eq!(
                known.len(),
                2,
                "one id names two room-scoped records: {known:?}"
            );
            let observed = known
                .iter()
                .find(|record| record.home == Some(home))
                .expect("scanned home record");
            assert_eq!(observed.id, scanned);
            assert!(observed.held_by.is_empty());
            assert_eq!(
                observed.package_id.as_deref(),
                Some("so.tilo.word-processor")
            );
            let referenced = known
                .iter()
                .find(|record| record.home == Some(other))
                .expect("other room reference");
            assert_eq!(referenced.id, scanned);
            assert_eq!(referenced.held_by, vec![other]);
            assert_eq!(referenced.title(), "report.odt");
        }

        // Phase `titles_name_the_thing_a_person_would_recognise`.
        {
            let file = RecordId::for_file("/home/q/books/dune.epub");
            assert_eq!(record_title(&file, &RecordKind::new("book")), "dune.epub");
            let markup = RecordId::derived("annotations", &file);
            assert_eq!(
                record_title(&markup, &RecordKind::new("annotation-set")),
                "annotations of dune.epub"
            );
            let minted = RecordId::minted();
            assert_eq!(
                record_title(&minted, &RecordKind::new("playlist")),
                "playlist"
            );
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps index guards alive through each generation-invalidation assertion"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn record_index_answers_fresh_stays_in_memory_and_refuses_exhausted_generations() {
        // Phase `every_change_is_visible_in_the_next_answer`.
        {
            let mut rr = RoomRecords::in_memory();
            let a = room();
            let b = room();
            let id = RecordId::minted();
            let kind = RecordKind::new("book");

            assert!(rr.known().is_empty(), "nothing yet");

            rr.reference(a, &id, &kind);
            assert_eq!(rr.known().len(), 1, "a reference is visible immediately");
            assert_eq!(rr.known()[0].held_by, vec![a]);

            rr.reference(b, &id, &kind);
            assert_eq!(rr.known().len(), 2, "the second room is a separate scope");
            assert!(rr.known().iter().any(|record| record.home == Some(a)));
            assert!(rr.known().iter().any(|record| record.home == Some(b)));

            rr.reference(b, &id, &RecordKind::new("field-guide"));
            assert_eq!(
                rr.known()
                    .iter()
                    .find(|record| record.home == Some(b))
                    .map(|record| record.kind.as_str()),
                Some("field-guide")
            );

            rr.unreference(a, &id);
            assert_eq!(
                rr.known()[0].held_by,
                vec![b],
                "dropping a reference is visible immediately"
            );

            rr.seed_observed([ObservedRecord {
                id: RecordId::for_file("/work/other.odt"),
                kind: RecordKind::new("document"),
                home: Some(a),
                package_id: "so.tilo.word-processor".into(),
            }]);
            assert_eq!(
                rr.known().len(),
                2,
                "a seeded record is visible immediately"
            );
        }

        // Phase `an_unchanged_index_hands_back_the_same_corpus`.
        {
            let mut rr = RoomRecords::in_memory();
            rr.reference(room(), &RecordId::minted(), &RecordKind::new("book"));
            let first = rr.known();
            let second = rr.known();
            assert!(
                std::rc::Rc::ptr_eq(&first, &second),
                "two reads with no change between them must not recompute"
            );
            rr.reference(room(), &RecordId::minted(), &RecordKind::new("book"));
            assert!(
                !std::rc::Rc::ptr_eq(&first, &rr.known()),
                "and a change must produce a new one"
            );
        }

        // Phase `an_in_memory_index_never_reaches_the_disk`.
        {
            let rr = RoomRecords::in_memory();
            assert!(
                rr.scan_complete(),
                "an in-memory index has no scan to wait for"
            );
            assert!(rr.known().is_empty());
        }

        // Phase `exhausted_generations_refuse_mutations_and_scan_replacements`.
        {
            let mut rr = RoomRecords::in_memory();
            let room = room();
            let old = RecordId::for_file("old.odt");
            let new = RecordId::for_file("new.odt");
            let kind = RecordKind::new("document");
            rr.generation = u64::MAX;
            let generation = rr.search_generation();
            assert!(!rr.reference(room, &old, &kind));
            rr.seed_observed([ObservedRecord {
                id: old.clone(),
                kind: kind.clone(),
                home: Some(room),
                package_id: "so.tilo.word-processor".into(),
            }]);
            assert!(rr.known().is_empty());
            assert_eq!(rr.search_generation(), generation);

            rr.generation = 0;
            rr.replace_scanned_records_for_test(vec![ObservedRecord {
                id: old.clone(),
                kind: kind.clone(),
                home: Some(room),
                package_id: "so.tilo.word-processor".into(),
            }]);
            let scan = Arc::clone(rr.scan.as_ref().unwrap());
            scan.lock().unwrap().revision = u64::MAX;
            rr.replace_scanned_records_for_test(vec![ObservedRecord {
                id: new,
                kind,
                home: Some(room),
                package_id: "so.tilo.word-processor".into(),
            }]);
            let scan = scan.lock().unwrap();
            assert_eq!(scan.records.len(), 1);
            assert_eq!(scan.records[0].id, old);
            assert_eq!(scan.revision, u64::MAX);
        }

        // Phase `validated_cold_load_is_read_only_and_checks_record_identity`.
        {
            let temp = tempfile::tempdir().unwrap();
            let record_dir = temp.path().join("records").join("one");
            std::fs::create_dir_all(&record_dir).unwrap();
            let document = record_dir.join(app_state::DOCUMENT_FILE);
            let expected = RecordId::for_file("cold.md");
            let mut doc = AutoCommit::new();
            ui::record::init(&mut doc, &expected, &RecordKind::new("note"), "");
            doc.put(&automerge::ROOT, "body", "cold history").unwrap();
            doc.commit_with(CommitOptions::default().with_time(10));
            std::fs::write(&document, doc.save()).unwrap();

            let root = temp.path().canonicalize().unwrap();
            let location = validate_record_location(&root, &document).unwrap();
            let mut loaded = read_validated_record_document(&expected, &location).unwrap();
            assert!(!loaded.get_heads().is_empty());
            assert!(
                read_validated_record_document(&RecordId::minted(), &location).is_none(),
                "a path may not be reused to answer for a different record"
            );
            assert!(
                !record_dir.join(app_state::LOCK_FILE).exists(),
                "history reads must not acquire or create a writer lock"
            );
        }
    }
}
