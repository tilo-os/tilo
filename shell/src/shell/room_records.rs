//! Which records a room holds, and which records exist at all.
//!
//! # Why the shell needs this
//!
//! A room starts empty (`plan/rooms.md`). That is only workable if moving
//! one of your own records into a room is a single gesture, and the gesture
//! the design picks is **search**: type the book's name, see it listed with
//! the room it lives in, press Enter, and it is here. Two facts have to be
//! available to answer that, and neither of them lived anywhere before:
//!
//! - **Which records exist**, so search has something to match against.
//! - **Which rooms hold which**, so a row can say where a record lives and
//!   so bringing it here can be *adding a reference* rather than a copy.
//!
//! # A room holds references, not copies
//!
//! Bringing a record into a room adds its id to that room's index and moves
//! no bytes: the record *is* one document, identified by its record id, and
//! an edit made through either room is an edit to that one document. Copying
//! is the separately-named verb ([`ui::record::room::duplicate`]) and stays
//! separate — two copies of a book diverge, and no amount of merging makes
//! them one book again.
//!
//! The index itself is [`ui::record::room`]: this module owns *where* a
//! room's index lives and *how it is found*, not what an index is.
//!
//! # The two halves of the corpus
//!
//! **Held** — the rooms' own indexes, one Automerge document per room under
//! this compositor's state directory. These are the references the user
//! made, and they are durable.
//!
//! **Observed** — the records apps have actually written. An app keeps one
//! document per record ([`ui::record::RecordSet`]), each stamped with its
//! identity, its type and the room it was minted in, so the store on disk is
//! already the answer to "which records exist"; this module reads it rather
//! than asking every app. The scan runs **once per process on a background
//! thread** and its result is shared by every surface that wants it — a
//! keystroke must never reach the filesystem, which is the constraint the
//! whole palette is built around.
//!
//! # The gap this leaves, stated plainly
//!
//! An app's own room index is its package-level document, and package state
//! is not partitioned per room on disk today: `~/.local/share/<package>/`
//! is one directory whichever room the app was launched into. So the
//! per-room index here is the compositor's, and it is genuinely per room;
//! what is *not* yet true is that an app launched into a room reads it. The
//! record's own `home` — which the app does stamp — is what carries the
//! room across that gap in the meantime, and it is why a record is
//! attributed to its home room even when no index references it.

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

/// Ceiling on a record document the scan will read.
///
/// A record's document holds the record's *content*, so it can be a whole
/// word-processor file. The scan only wants three fields out of the header,
/// but automerge has no way to read a document in part, so a document past
/// this size is skipped rather than paged into the compositor. Skipping
/// costs that record its search row; loading it could cost the session its
/// memory, and the second failure is much worse than the first.
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

    /// What to call this record in a list.
    ///
    /// A file record is named by its file, which is the whole point of a
    /// path being an identity. A derived record is named by its role and
    /// its target ("annotations of book.pdf") because that is the only
    /// thing about it a person would recognise. A minted record with no
    /// natural name falls back to its type — honest, and still the thing
    /// the user would type.
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
}

impl RoomRecords {
    /// The compositor's own indexes, loaded from disk, with the record scan
    /// running behind them.
    ///
    /// Reads: every room index this compositor has written. Does not write:
    /// a room gains a file the first time something is referenced into it,
    /// so a session that brings nothing anywhere leaves no state behind.
    pub fn open() -> Self {
        let mut me = Self {
            held: BTreeMap::new(),
            scan: Some(shared_scan()),
            seeded: Vec::new(),
            persistent: true,
        };
        me.load_indexes();
        me
    }

    /// An instance that never touches the disk. For tests, for `--shots`,
    /// and for any run that must not leave state behind.
    pub fn in_memory() -> Self {
        Self {
            held: BTreeMap::new(),
            scan: None,
            seeded: Vec::new(),
            persistent: false,
        }
    }

    /// Add records to the known corpus without a scan. Used by tests, and
    /// by any surface that learns of a record first-hand.
    pub fn seed_observed(&mut self, records: impl IntoIterator<Item = ObservedRecord>) {
        self.seeded.extend(records);
    }

    /// Bring a record into `room`.
    ///
    /// Adds a reference — the room gains the record's id, and the record's
    /// document is untouched. Returns whether this added something, so a
    /// caller can tell "brought here" from "was already here" without
    /// asking twice.
    pub fn reference(&mut self, room: RoomId, id: &RecordId, kind: &RecordKind) -> bool {
        let slot = self.index_mut(room);
        let added = room::reference(&mut slot.doc, id, kind);
        if added {
            if let Some(saver) = slot.autosave.as_mut() {
                saver.request(&mut slot.doc);
            }
        }
        added
    }

    /// Drop a room's reference to a record. The record itself is untouched:
    /// it may be referenced by other rooms, and it is the same document in
    /// each.
    ///
    /// Removal stops future changes flowing to this room's members. It
    /// cannot un-share bytes a peer already synced, and nothing here should
    /// imply otherwise.
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
    /// indexes and from the records apps have written, sorted by id so the
    /// answer does not depend on map iteration order.
    ///
    /// In-memory only: no disk access, no CRDT load. This runs per
    /// keystroke.
    pub fn known(&self) -> Vec<KnownRecord> {
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
                let entry = by_id
                    .entry(checkout.id.clone())
                    .or_insert_with(|| KnownRecord {
                        id: checkout.id.clone(),
                        kind: checkout.kind.clone(),
                        home: None,
                        held_by: Vec::new(),
                        package_id: None,
                    });
                if entry.kind.as_str().is_empty() {
                    entry.kind = checkout.kind.clone();
                }
                if !entry.held_by.contains(room) {
                    entry.held_by.push(*room);
                }
            }
        }
        for r in by_id.values_mut() {
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
            let mut doc = AutoCommit::new();
            let autosave = open_room_store(room, &mut doc);
            self.held.insert(room, Held { doc, autosave });
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

/// The process-wide record scan, started on first use.
///
/// One walk per process, whoever asks: the compositor builds a palette host
/// per session but tests build many, and a scan per host would read every
/// app's store that many times.
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

/// Walk every package's record store and read each record's identity.
///
/// Deliberately shallow: `<data home>/<package>/records/<key>/state.automerge`
/// and nothing else, so the walk is bounded by the number of records rather
/// than by the size of the user's home.
fn scan_record_stores(root: &Path) -> Vec<ObservedRecord> {
    let mut out = Vec::new();
    let Ok(packages) = std::fs::read_dir(root) else {
        return out;
    };
    for package in packages.flatten() {
        let Some(package_id) = package.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if package_id == SHELL_PACKAGE_ID {
            continue;
        }
        let records = package.path().join(app_state::RECORDS_DIR);
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
    out
}

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

/// Human name for the room a record lives in, resolved against the live
/// workspace tree.
///
/// A room the tree does not know about is named as an unknown room rather
/// than by its uuid: a raw uuid in a palette row tells the reader nothing
/// and looks like a bug.
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
