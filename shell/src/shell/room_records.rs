//! Which records a room holds, and which records exist at all — the two
//! facts search needs to move a record into a room in one gesture.
//!
//! A room holds **references, not copies**: bringing a record in adds its id
//! to that room's index and moves no bytes, so an edit through either room
//! edits the one document. Copying is the separately-named
//! [`ui::record::room::duplicate`]. The index itself is [`ui::record::room`];
//! this module owns only *where* a room's index lives and how it is found.
//!
//! The corpus has two halves. **Held** is the rooms' own indexes, one
//! Automerge document per room under the compositor's state directory.
//! **Observed** is the records apps have written — an app keeps one
//! document per record ([`ui::record::RecordSet`]) stamped with its
//! identity, type and home room, so the store on disk already answers which
//! records exist. That scan runs once per process on a background thread
//! and is shared by every surface: a keystroke must never reach the filesystem.
//!
//! Known gap: package state is not partitioned per room on disk
//! (`~/.local/share/<package>/` is one directory whichever room the app was
//! launched into), so the per-room index here is genuinely per room but an
//! app launched into a room does not yet read it. A record's own `home`
//! carries the room across that gap, which is why a record is attributed to
//! its home room even when no index references it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use ui::record::{RecordCheckout, RecordId, RecordKind, room};

use crate::shell::state::RoomId;

/// Package id the compositor stores its own room indexes under. Not an
/// installed package — it is the same directory shape so the same store,
/// with its atomic writes and its single-writer lock, can be reused.
pub const SHELL_PACKAGE_ID: &str = "dev.qos.shell";

/// Ceiling on a record document the scan will read. A record's document
/// holds its *content*, and automerge cannot read a document in part, so
/// anything larger is skipped rather than paged into the compositor —
/// losing a search row is much cheaper than losing the session to memory.
const MAX_RECORD_DOC_BYTES: u64 = 16 * 1024 * 1024;

/// One record the compositor knows about, and where it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownRecord {
    pub id: RecordId,
    pub kind: RecordKind,
    /// The room the record was minted in, as its own document records it.
    /// `None` for a record whose document names no home, and for one the
    /// compositor only knows about because a room references it.
    pub home: Option<RoomId>,
    /// Rooms whose index references this record. Sorted.
    pub held_by: Vec<RoomId>,
    /// The package whose store holds this record's document, when the
    /// record was observed there.
    pub package_id: Option<String>,
}

impl KnownRecord {
    /// Every room this record is reachable from: the one it was minted in
    /// plus every room that has referenced it.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut out: Vec<RoomId> = self.held_by.clone();
        if let Some(home) = self.home {
            if !out.contains(&home) {
                out.push(home);
            }
        }
        out.sort();
        out
    }

    /// Is this record in `room` — because it was minted there or because
    /// that room references it?
    pub fn is_in(&self, room: RoomId) -> bool {
        self.home == Some(room) || self.held_by.contains(&room)
    }

    /// What to call this record in a list: a file record by its file, a
    /// derived one by role and target ("annotations of book.pdf"), and a
    /// minted one with no natural name by its type.
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
    /// `None` for an in-memory index and for a room whose store could not
    /// be opened — reported once, then the room runs without durability
    /// rather than the compositor refusing to reference anything.
    autosave: Option<app_state::Autosave>,
}

#[derive(Default)]
struct ScanState {
    records: Vec<ObservedRecord>,
    /// Whether the walk has finished. A query before it does gets whatever
    /// is known so far rather than blocking a keystroke on the disk.
    done: bool,
}

/// What [`RoomRecords::known`] last computed, and what from. Reading a
/// room's index walks an Automerge map and fetches every entry — 19 ms at
/// 300 records, against a whole-palette budget of a few — so the answer is
/// cached, keyed on the only two things that change it: a new reference and
/// the scan landing.
struct KnownCache {
    /// `(index generation, scanned record count, scan finished)`.
    key: (u64, usize, bool),
    records: std::rc::Rc<Vec<KnownRecord>>,
}

/// The rooms' record indexes, and the records that exist.
pub struct RoomRecords {
    held: BTreeMap<RoomId, Held>,
    /// Shared with the background scan. `None` for an in-memory instance,
    /// which never reads the disk.
    scan: Option<Arc<Mutex<ScanState>>>,
    /// Records seeded directly rather than scanned — how a test supplies a
    /// corpus, and how a caller that already knows about a record (it just
    /// opened one) can say so without waiting for the next walk.
    seeded: Vec<ObservedRecord>,
    persistent: bool,
    /// The type each record was last labelled with, in call order.
    ///
    /// A type belongs to the **record**, not to a room's reference to it, so
    /// it cannot be derived by merging room indexes: `RoomId` is a random v4
    /// uuid, so `BTreeMap` order would decide which label won when two rooms
    /// referenced one record under different types, and that changes run to
    /// run. Last label written is deterministic and is what re-referencing
    /// means. A record observed on disk overrides this — that kind is the
    /// record speaking rather than a room describing it.
    labelled: BTreeMap<RecordId, RecordKind>,
    /// Bumped by every mutation that could change what [`Self::known`]
    /// answers. Incremented in one place ([`Self::changed`]) rather than at
    /// each call site: a counter that misses a path yields a silently stale
    /// cache, and nothing tells the user a wrong search result is wrong.
    generation: u64,
    /// The [`Self::generation`] last mirrored into the workspace tree, so
    /// [`Self::replicate_with`] can do nothing on frames where nothing
    /// moved. `None` until the first exchange, which is what makes a session
    /// publish what it already held rather than only later additions.
    replicated_generation: Option<u64>,
    known: std::cell::RefCell<Option<KnownCache>>,
}

impl RoomRecords {
    /// An index that never touches the disk — what everything is built with.
    /// Durability is opt-in via [`Self::attach_store`], so a test, `--shots`
    /// run or headless harness cannot write into the user's own state merely
    /// by constructing a compositor.
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

    /// Something changed that [`Self::known`] would answer differently.
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Make this index durable and start looking for the records apps have
    /// written. Reads every room index written before; writes nothing — a
    /// room gains a file only when something is referenced into it, so a
    /// session that brings nothing anywhere leaves nothing behind.
    pub fn attach_store(&mut self) {
        if self.persistent {
            return;
        }
        self.persistent = true;
        self.scan = Some(shared_scan());
        self.changed();
        // References made before attachment become durable too — dropping
        // them would make the moment of attachment a silent data loss.
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

    /// Add records to the known corpus without a scan. Used by tests, and
    /// by any surface that learns of a record first-hand.
    pub fn seed_observed(&mut self, records: impl IntoIterator<Item = ObservedRecord>) {
        self.seeded.extend(records);
        self.changed();
    }

    /// Bring a record into `room` by reference: the room gains the record's
    /// id and the record's document is untouched. Returns whether this added
    /// something, so a caller can tell "brought here" from "already here".
    pub fn reference(&mut self, room: RoomId, id: &RecordId, kind: &RecordKind) -> bool {
        let slot = self.index_mut(room);
        let added = room::reference(&mut slot.doc, id, kind);
        if added {
            if let Some(saver) = slot.autosave.as_mut() {
                saver.request(&mut slot.doc);
            }
        }
        // Unconditionally, not only when `added`: re-referencing an
        // existing record refreshes its type, which is part of the answer.
        self.labelled.insert(id.clone(), kind.clone());
        self.changed();
        added
    }

    /// Drop a room's reference to a record; the record itself is untouched
    /// and may be referenced by other rooms. This stops future changes
    /// flowing to the room's members but cannot un-share bytes a peer has
    /// already synced.
    pub fn unreference(&mut self, room: RoomId, id: &RecordId) -> bool {
        let Some(slot) = self.held.get_mut(&room) else {
            return false;
        };
        let dropped = room::unreference(&mut slot.doc, id);
        if dropped {
            if let Some(saver) = slot.autosave.as_mut() {
                saver.request(&mut slot.doc);
            }
        }
        self.changed();
        dropped
    }

    /// Does `room`'s index reference `id`?
    pub fn holds(&self, room: RoomId, id: &RecordId) -> bool {
        self.held
            .get(&room)
            .map(|h| room::holds(&h.doc, id))
            .unwrap_or(false)
    }

    /// Every record `room`'s index references.
    pub fn records_in(&self, room: RoomId) -> Vec<RecordCheckout> {
        self.held
            .get(&room)
            .map(|h| room::references(&h.doc))
            .unwrap_or_default()
    }

    /// Every record this compositor knows about, merged from the rooms'
    /// indexes and the records apps have written, sorted by id so the answer
    /// does not depend on map iteration order.
    ///
    /// **Runs on every keystroke**, so it does no disk access and, after the
    /// first call, no CRDT reads: walking a room's index measured 19 ms over
    /// 300 records, longer than a frame. Freshness comes from
    /// [`Self::changed`] plus the scan's progress — the only two things that
    /// can move the answer.
    pub fn known(&self) -> std::rc::Rc<Vec<KnownRecord>> {
        let scan_key = self
            .scan
            .as_ref()
            .and_then(|s| s.lock().ok().map(|st| (st.records.len(), st.done)))
            .unwrap_or((0, true));
        let key = (self.generation, scan_key.0, scan_key.1);
        if let Some(cached) = self.known.borrow().as_ref() {
            if cached.key == key {
                return std::rc::Rc::clone(&cached.records);
            }
        }
        let records = std::rc::Rc::new(self.compute_known());
        *self.known.borrow_mut() = Some(KnownCache {
            key,
            records: std::rc::Rc::clone(&records),
        });
        records
    }

    fn compute_known(&self) -> Vec<KnownRecord> {
        let mut by_id: BTreeMap<RecordId, KnownRecord> = BTreeMap::new();

        let mut observe = |o: &ObservedRecord| {
            let entry = by_id.entry(o.id.clone()).or_insert_with(|| KnownRecord {
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
        if let Some(scan) = self.scan.as_ref() {
            if let Ok(state) = scan.lock() {
                state.records.iter().for_each(&mut observe);
            }
        }
        self.seeded.iter().for_each(&mut observe);

        for (room, held) in &self.held {
            for checkout in room::references(&held.doc) {
                // Deliberately empty: a room says *that* it holds a record,
                // not what the record is. The type is filled in below from
                // the record's own word, or from the last label a reference
                // supplied — never from whichever room sorted first.
                let entry = by_id
                    .entry(checkout.id.clone())
                    .or_insert_with(|| KnownRecord {
                        id: checkout.id.clone(),
                        kind: RecordKind::new(""),
                        home: None,
                        held_by: Vec::new(),
                        package_id: None,
                    });
                if !entry.held_by.contains(room) {
                    entry.held_by.push(*room);
                }
            }
        }
        // The room pass contributes which rooms hold what, never the type:
        // a room describing a record cannot rename it. An observed kind (the
        // app's own word) stands; otherwise the last label a reference gave.
        for (id, r) in by_id.iter_mut() {
            if r.kind.as_str().is_empty()
                && let Some(k) = self.labelled.get(id)
            {
                r.kind = k.clone();
            }
            r.held_by.sort();
        }
        by_id.into_values().collect()
    }

    /// Whether the background scan has finished. Surfaces that want to say
    /// "still looking" read this; nothing waits on it.
    pub fn scan_complete(&self) -> bool {
        self.scan
            .as_ref()
            .map(|s| s.lock().map(|st| st.done).unwrap_or(false))
            .unwrap_or(true)
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

    /// Exchange this machine's room indexes with the peer-replication path,
    /// returning whether anything arrived from a peer.
    ///
    /// The shell does this rather than the tree because a room's index is an
    /// Automerge document behind an `Rc<RefCell<_>>` on the overlay host
    /// while replication holds the tree across an `Arc<RwLock<_>>` on a
    /// `Send` boundary — neither can hold the other. The tree carries a
    /// plain-data mirror
    /// ([`crate::shell::state::WorkspaceTree::room_records_held`]) and this
    /// is where the two are reconciled, from the frame pump that holds both.
    ///
    /// Only references cross. A room learns which records it holds, never
    /// who has one open, which window, or where anyone scrolled — none of
    /// that is a fact about the room, which is why the index rather than the
    /// window layout is the unit.
    pub fn replicate_with(&mut self, tree: &mut crate::shell::state::WorkspaceTree) -> bool {
        // Reading a room's index walks an Automerge map, too expensive per
        // frame (see `known`), so publish only when a mutation moved the answer.
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

        // Fold in arrivals. `reference` is idempotent, so a record a peer
        // re-announces is not an edit here; what it does do is refresh the
        // record's type, which is what re-referencing means.
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
        // The publish above ran against the pre-merge generation, so what
        // just arrived would wait for the next local change. Re-arm instead:
        // a guest that learned of a record is a legitimate source for the
        // next peer, which is what lets a room converge when its members are
        // not all connected at once.
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

    /// Load every room index already on disk. Rooms already live in
    /// memory are left alone — [`Self::index_mut`] opened their store when
    /// the store was attached, which merged the same bytes in.
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
        }
    }
}

impl Default for RoomRecords {
    fn default() -> Self {
        Self::in_memory()
    }
}

/// Open (and merge in) the stored index for `room`, returning its writer.
fn open_room_store(room: RoomId, doc: &mut AutoCommit) -> Option<app_state::Autosave> {
    let mut store = match app_state::DocStore::open_room(SHELL_PACKAGE_ID, &room.to_string()) {
        Ok(store) => store,
        Err(e) => {
            tracing::error!(
                room = %room,
                error = %e,
                "room index store unavailable; this room's references are not persisted"
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
    let mut autosave = app_state::Autosave::spawn(store, app_state::DEFAULT_DEBOUNCE);
    if let Some(mut stored) = stored {
        autosave.seed(&mut stored);
        if let Err(e) = doc.merge(&mut stored) {
            tracing::error!(room = %room, error = %e, "stored room index could not be merged");
        }
    }
    Some(autosave)
}

fn rooms_dir() -> PathBuf {
    cli::standalone_state_dir(SHELL_PACKAGE_ID).join(app_state::ROOMS_DIR)
}

/// The process-wide record scan, started on first use. One walk per process
/// whoever asks — tests build many palette hosts, and a scan per host would
/// read every app's store that many times.
fn shared_scan() -> Arc<Mutex<ScanState>> {
    static SCAN: OnceLock<Arc<Mutex<ScanState>>> = OnceLock::new();
    SCAN.get_or_init(|| {
        let state = Arc::new(Mutex::new(ScanState::default()));
        let handle = Arc::clone(&state);
        let root = cli::data_home();
        std::thread::Builder::new()
            .name("qos-record-scan".into())
            .spawn(move || {
                let found = scan_record_stores(&root);
                if let Ok(mut st) = handle.lock() {
                    st.records = found;
                    st.done = true;
                }
            })
            .ok();
        state
    })
    .clone()
}

/// Walk every room's packages and read each record's identity. The
/// compositor is the only component that reads across rooms and must:
/// Super+K shows records from the user's other rooms, which an app cannot do
/// because its sandbox contains exactly one room's directory.
///
/// Deliberately shallow —
/// `<data home>/qos-rooms/<room>/<package>/.local/share/<package>/records/<key>/state.automerge`
/// and nothing else — so the walk is bounded by the number of records rather
/// than the size of the user's home. The middle segment is the sandbox's
/// `XDG_DATA_HOME`, where `cli::standalone_state_dir` resolves under the launcher.
fn scan_record_stores(root: &Path) -> Vec<ObservedRecord> {
    let mut out = Vec::new();
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
            // Inside the sandbox this is `$XDG_DATA_HOME/<package>`; from
            // out here it is that path spelled against the host.
            let records = package
                .path()
                .join(&*SANDBOX_DATA_SUBDIR)
                .join(&package_id)
                .join(app_state::RECORDS_DIR);
            let Ok(entries) = std::fs::read_dir(&records) else {
                continue;
            };
            for entry in entries.flatten() {
                let doc_path = entry.path().join(app_state::DOCUMENT_FILE);
                match std::fs::metadata(&doc_path) {
                    Ok(meta) if meta.len() <= MAX_RECORD_DOC_BYTES => {}
                    Ok(meta) => {
                        tracing::debug!(
                            path = %doc_path.display(),
                            bytes = meta.len(),
                            "record document too large to index for search"
                        );
                        continue;
                    }
                    Err(_) => continue,
                }
                let Some(observed) = read_record_header(&doc_path, &package_id) else {
                    continue;
                };
                out.push(observed);
            }
        }
    }
    out
}

/// The path `XDG_DATA_HOME` takes inside an app's directory. Derived from
/// the launcher's own announcement rather than written out again here, so
/// the walk cannot drift from where apps actually write.
static SANDBOX_DATA_SUBDIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
    let root = Path::new(launcher::appdir::SANDBOX_APP_DIR);
    launcher::appdir::sandbox_env()
        .into_iter()
        .find(|(k, _)| k == "XDG_DATA_HOME")
        .and_then(|(_, v)| {
            Path::new(&v)
                .strip_prefix(root)
                .ok()
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from(".local/share"))
});

/// Read one record document's identity, type and home room.
fn read_record_header(path: &Path, package_id: &str) -> Option<ObservedRecord> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    // A trailing partially-written change chunk is dropped rather than
    // failing the read — the same policy the store itself loads under.
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
    if let Some(rest) = id.as_str().strip_prefix("derived:") {
        if let Some((role, target)) = rest.split_once('@') {
            let target_name = RecordId::parse(target)
                .map(|t| record_title(&t, &RecordKind::new("")))
                .unwrap_or_else(|| target.to_string());
            return format!("{role} of {target_name}");
        }
    }
    if !kind.as_str().is_empty() {
        return kind.as_str().to_string();
    }
    id.as_str().to_string()
}

fn leaf_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Human name for the room a record lives in. A room the tree does not know
/// is named as unknown rather than by its uuid — a raw uuid in a palette row
/// tells the reader nothing and looks like a bug.
pub fn room_label(tree: &crate::shell::state::WorkspaceTree, room: RoomId) -> Option<String> {
    tree.workspaces
        .iter()
        .find(|w| w.room == room)
        .map(|w| w.name.clone())
}

/// Rooms in the tree, most-recently-active first is not knowable here, so:
/// tree order. Used to pick which of several rooms a record's row names.
pub fn known_rooms(tree: &crate::shell::state::WorkspaceTree) -> BTreeSet<RoomId> {
    tree.workspaces.iter().map(|w| w.room).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomId {
        RoomId::new()
    }

    #[test]
    fn a_room_holds_references_and_the_record_is_untouched() {
        let mut rr = RoomRecords::in_memory();
        let a = room();
        let b = room();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");

        assert!(rr.reference(a, &id, &kind), "first reference adds");
        assert!(!rr.reference(a, &id, &kind), "referencing twice adds nothing");
        assert!(rr.holds(a, &id));
        assert!(!rr.holds(b, &id));

        // The same id in a second room: one record, two references.
        assert!(rr.reference(b, &id, &kind));
        assert!(rr.holds(a, &id) && rr.holds(b, &id));
        assert_eq!(rr.records_in(a), rr.records_in(b));

        // Dropping one room's reference leaves the other alone.
        assert!(rr.unreference(a, &id));
        assert!(!rr.holds(a, &id));
        assert!(rr.holds(b, &id), "the record is still in the other room");
    }

    #[test]
    fn known_merges_what_rooms_hold_with_what_apps_wrote() {
        let mut rr = RoomRecords::in_memory();
        let home = room();
        let other = room();
        let scanned = RecordId::for_file("/work/report.odt");
        rr.seed_observed([ObservedRecord {
            id: scanned.clone(),
            kind: RecordKind::new("document"),
            home: Some(home),
            package_id: "dev.qos.word-processor".into(),
        }]);
        rr.reference(other, &scanned, &RecordKind::new("document"));

        let known = rr.known();
        assert_eq!(known.len(), 1, "one record, two facts about it: {known:?}");
        let r = &known[0];
        assert_eq!(r.id, scanned);
        assert_eq!(r.home, Some(home));
        assert_eq!(r.held_by, vec![other]);
        assert!(r.is_in(home) && r.is_in(other));
        assert_eq!(r.package_id.as_deref(), Some("dev.qos.word-processor"));
        assert_eq!(r.title(), "report.odt");
    }

    #[test]
    fn titles_name_the_thing_a_person_would_recognise() {
        let file = RecordId::for_file("/home/q/books/dune.epub");
        assert_eq!(record_title(&file, &RecordKind::new("book")), "dune.epub");
        let markup = RecordId::derived("annotations", &file);
        assert_eq!(
            record_title(&markup, &RecordKind::new("annotation-set")),
            "annotations of dune.epub"
        );
        let minted = RecordId::minted();
        assert_eq!(record_title(&minted, &RecordKind::new("playlist")), "playlist");
    }

    /// The cached corpus must never survive a change to what it answers —
    /// nothing on screen says a stale search result is wrong. Every mutation
    /// is exercised here and must be visible in the very next `known()`.
    #[test]
    fn every_change_is_visible_in_the_next_answer() {
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
        assert_eq!(
            rr.known()[0].rooms(),
            {
                let mut r = vec![a, b];
                r.sort();
                r
            },
            "the second room is visible immediately"
        );

        // A record re-referenced with a different type: same entry, and
        // the change still has to land — this is the path that returns
        // `false` from `reference`, and keying the cache on that return
        // value would have missed it.
        rr.reference(b, &id, &RecordKind::new("field-guide"));
        assert_eq!(rr.known()[0].kind.as_str(), "field-guide");

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
            package_id: "dev.qos.word-processor".into(),
        }]);
        assert_eq!(rr.known().len(), 2, "a seeded record is visible immediately");
    }

    /// The corpus is shared rather than rebuilt when nothing changed —
    /// which is what keeps a keystroke off the CRDT.
    #[test]
    fn an_unchanged_index_hands_back_the_same_corpus() {
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

    #[test]
    fn an_in_memory_index_never_reaches_the_disk() {
        let rr = RoomRecords::in_memory();
        assert!(
            rr.scan_complete(),
            "an in-memory index has no scan to wait for"
        );
        assert!(rr.known().is_empty());
    }
}
