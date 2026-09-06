//! Records — the unit of state, collaboration and storage. A record has a
//! type, a stable identity, and a home room. A file's identity is its path
//! — the unification, not a special case. Records reference other records
//! rather than nesting (annotations reference the document they mark up),
//! so "share the book without my notes" is expressible.
//!
//! A record id is scoped by the room that hydrates it. Rooms hold references
//! ([`room::reference`]), but an identical id in another room gets a distinct
//! local document and collaboration route; explicit export/import is required
//! to move content across that authority boundary.
//!
//! One document per record is forced, not chosen: an Automerge change graph
//! names its dependencies by hash, so a filtered stream leaves the receiver
//! holding orphaned changes — it cannot be served in part.
//!
//! [`RecordSet`] owns the open records and hands the active one to the
//! toolkit; [`tick`] parks the outgoing document and checks the named one
//! in each frame. Not all app state is a record — a theme or account stays
//! in the package-level document ([`crate::persist`]) instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

/// Key, in a record's own document, of the map holding its identity.
const META: &str = "qos:record";
/// Key in a record document for the room that owns this local replica.
/// `RecordId` is a user/application name; it is not an authority boundary.
const ROOM: &str = "room";
/// Key, in a room's index document, of the map of record references.
const INDEX: &str = "qos:records";

/// The stable identity of a record. Three shapes, encoded by prefix:
/// `file:<absolute path>` (identity is the path — opening it twice reaches
/// one record), `uuid:<uuid>` (minted, for a record with no natural name),
/// and `tab:<tab id>` (a window's scratch document until a file names it,
/// see [`RecordId::for_tab`]).
///
/// The prefix is part of the id, not a tag beside it: the id travels on the
/// wire and names a file on disk, so it must be self-describing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(String);

impl RecordId {
    /// The record a file *is*. Relative paths resolve against the cwd, so
    /// two spellings of one file are one record.
    ///
    /// The id embeds the absolute path, so two peers reach one record only
    /// if the file sits at the same path on both. Don't use this for
    /// content embedded in a document (a dropped image) — that needs a
    /// content-addressed record, not a path-addressed one.
    pub fn for_file(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| p.to_path_buf(), |cwd| cwd.join(p))
        };
        // Not `canonicalize`: it fails for a file that doesn't exist yet
        // (Save As), and a record must be nameable before its bytes are.
        Self(format!("file:{}", normalise(&abs).display()))
    }

    pub fn minted() -> Self {
        Self(format!("uuid:{}", uuid::Uuid::new_v4()))
    }

    /// Reuses the tab id rather than minting a new one: it's already unique
    /// per machine and persisted with the window tree, so a separate id
    /// beside it could only drift.
    pub fn for_tab(tab: u64) -> Self {
        Self(format!("tab:{tab:016x}"))
    }

    /// Identity of the record that plays `role` for `of` — "the annotations
    /// of this document". Distinct from its target so the two can be
    /// shared separately: handing over the document doesn't hand the notes.
    pub fn derived(role: &str, of: &RecordId) -> Self {
        Self(format!("derived:{role}@{}", of.0))
    }

    /// Adopt an id read back from a document, a room index, or the wire.
    /// `None` for the empty string, which addresses nothing.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        Some(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path a file record names, if it is one.
    pub fn file_path(&self) -> Option<PathBuf> {
        self.0.strip_prefix("file:").map(PathBuf::from)
    }

    /// A filesystem-safe directory name for this record's document. Must be
    /// stable (same record → same directory next launch, or data is
    /// silently orphaned) and collision-free (two records sharing a
    /// directory merge their documents) — hence a readable sanitised prefix
    /// plus a hash of the whole id, which is what actually distinguishes
    /// them.
    pub fn storage_key(&self) -> String {
        let mut prefix = String::with_capacity(48);
        for c in self.0.chars() {
            if prefix.len() >= 40 {
                break;
            }
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                prefix.push(c);
            } else {
                prefix.push('_');
            }
        }
        format!("{prefix}-{:016x}", fnv1a64(self.0.as_bytes()))
    }
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Collapse `.` and `..` without touching the filesystem (`canonicalize`
/// needs the file to exist; a record must be nameable before its bytes
/// are). A surviving `..` would let two spellings of one path be two
/// records, and let a record key address a directory outside app state.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// FNV-1a, 64-bit. Written out rather than imported: this hash names
/// directories on disk, so it must be stable across every future crate
/// version — a guarantee a library's default hasher does not make.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What kind of thing a record is — `document`, `annotation-set`, `book`,
/// `playlist`. App-owned rather than system-registered, so an app can add
/// a kind without coordinating; the cost is no cross-app library view yet
/// (needs a system-owned type registry, not built here).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordKind(String);

impl RecordKind {
    pub fn new(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One record's reference to another: annotations → the document they mark up,
/// a duplicate → the record it was copied from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRef {
    pub target: RecordId,
    /// What the reference means to the app — `marks-up`, `copied-from`,
    /// `attached-to`. Free-form for the same reason [`RecordKind`] is.
    pub relation: String,
}

impl RecordRef {
    pub fn new(target: RecordId, relation: impl Into<String>) -> Self {
        Self {
            target,
            relation: relation.into(),
        }
    }
}

/// A record named together with its type — what a widget declares it is
/// working on, and what a room index holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordCheckout {
    pub id: RecordId,
    pub kind: RecordKind,
    /// Records this one references, and what each means. Declared here
    /// (not written by the app) because a reference has two halves only
    /// the host can write: it goes into this record's document, and the
    /// target is brought into the room index so it doesn't dangle. One
    /// level deep.
    pub references: Vec<(RecordCheckout, String)>,
}

impl RecordCheckout {
    pub fn new(id: RecordId, kind: RecordKind) -> Self {
        Self {
            id,
            kind,
            references: Vec::new(),
        }
    }

    #[must_use]
    pub fn referencing(mut self, target: RecordCheckout, relation: impl Into<String>) -> Self {
        self.references.push((target, relation.into()));
        self
    }
}

/// `None` only if `put_object` on the document root itself fails, which
/// does not happen for a document this call has exclusive access to.
fn meta_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    match doc.get(&ROOT, META) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => doc.put_object(&ROOT, META, ObjType::Map).ok(),
    }
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Stamp a document with the identity of the record it holds. Idempotent:
/// re-opening leaves an existing stamp alone, so a restart doesn't author
/// a change graph entry that says nothing new.
pub fn init(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind, home: &str) {
    let _ = init_scoped(doc, id, kind, home, None);
}

/// Bind a record document to `room`.  Unlike the historical [`init`], this
/// never rewrites an existing identity or room: accepting a document stamped
/// for another room would turn a matching record id into a cross-room alias.
/// A room-less legacy document may be adopted exactly once after its identity
/// has been checked.
pub fn init_scoped(
    doc: &mut AutoCommit,
    id: &RecordId,
    kind: &RecordKind,
    home: &str,
    room: Option<&str>,
) -> bool {
    let Some(m) = meta_obj(doc) else {
        // The document's root object is gone — nothing to stamp.
        return false;
    };
    if str_at(doc, &m, "id").is_some_and(|existing| existing != id.as_str())
        || str_at(doc, &m, "kind").is_some_and(|existing| existing != kind.as_str())
        || room.is_some_and(|scope| str_at(doc, &m, ROOM).is_some_and(|existing| existing != scope))
    {
        return false;
    }
    if str_at(doc, &m, "id").is_none() {
        let _ = doc.put(&m, "id", id.as_str());
    }
    if str_at(doc, &m, "kind").is_none() {
        let _ = doc.put(&m, "kind", kind.as_str());
    }
    // Home is the room that minted the record; never rewritten on a later
    // open, or the last room to open it would become the owner.
    if str_at(doc, &m, "home").is_none() {
        let _ = doc.put(&m, "home", home);
    }
    if let Some(room) = room
        && str_at(doc, &m, ROOM).is_none()
    {
        let _ = doc.put(&m, ROOM, room);
    }
    if !matches!(
        doc.get(&m, "refs"),
        Ok(Some((Value::Object(ObjType::List), _)))
    ) {
        let _ = doc.put_object(&m, "refs", ObjType::List);
    }
    true
}

/// The identity stamped into `doc`, if it holds a record.
pub fn identity(doc: &AutoCommit) -> Option<RecordCheckout> {
    let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&ROOT, META) else {
        return None;
    };
    let id = RecordId::parse(&str_at(doc, &m, "id")?)?;
    let kind = RecordKind::new(str_at(doc, &m, "kind").unwrap_or_default());
    Some(RecordCheckout::new(id, kind))
}

/// The room this record was minted in.
pub fn home(doc: &AutoCommit) -> Option<String> {
    let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&ROOT, META) else {
        return None;
    };
    str_at(doc, &m, "home")
}

/// The room this local record replica is bound to.  Absent only for records
/// written before room-partitioned persistence; callers may adopt those once
/// with [`init_scoped`], but must never treat an explicit different room as
/// interchangeable.
pub fn room_scope(doc: &AutoCommit) -> Option<String> {
    let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&ROOT, META) else {
        return None;
    };
    str_at(doc, &m, ROOM)
}

/// `None` only if the containing document's root/meta object is gone; see
/// [`meta_obj`].
fn refs_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    let m = meta_obj(doc)?;
    match doc.get(&m, "refs") {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => doc.put_object(&m, "refs", ObjType::List).ok(),
    }
}

/// Idempotent on `(target, relation)`: a reference is a fact, not an
/// event.
pub fn add_reference(doc: &mut AutoCommit, r: &RecordRef) -> bool {
    if references(doc).iter().any(|e| e == r) {
        return false;
    }
    let Some(list) = refs_obj(doc) else {
        return false;
    };
    let n = doc.length(&list);
    let Ok(entry) = doc.insert_object(&list, n, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&entry, "target", r.target.as_str());
    let _ = doc.put(&entry, "relation", r.relation.as_str());
    true
}

/// Every record this one references.
pub fn references(doc: &AutoCommit) -> Vec<RecordRef> {
    let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&ROOT, META) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&m, "refs") else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, i) else {
                return None;
            };
            let target = RecordId::parse(&str_at(doc, &entry, "target")?)?;
            Some(RecordRef {
                target,
                relation: str_at(doc, &entry, "relation").unwrap_or_default(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The room index: which records a room holds.
// ---------------------------------------------------------------------------

/// A room's index of the records it holds. Ownership of the index document
/// is the caller's business — the compositor keeps one per room
/// (`shell::room_records`).
///
/// Known gap: an app launched into a room does not yet read that room's
/// index; today a record is attributed to its room via its own `home`
/// stamp. Closing the gap means partitioning app state per room.
///
/// **The reference is what grants access.** A member can reach only the
/// records the room references. Removing a reference stops future changes
/// flowing; it cannot un-share bytes a peer already synced.
pub mod room {
    use super::{
        AutoCommit, INDEX, ObjId, ObjType, ROOT, ReadDoc, RecordCheckout, RecordId, RecordKind,
        RecordSet, Transactable, Value, str_at,
    };

    /// `None` only if `put_object` on the document root itself fails, which
    /// does not happen for a document this call has exclusive access to.
    fn index_obj(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, INDEX) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, INDEX, ObjType::Map).ok(),
        }
    }

    fn index_ro(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, INDEX) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// Bring a record name into this room. Keyed by record id (not appended
    /// to a list) so two machines referencing the same name concurrently
    /// converge on one index entry. The document it resolves to remains
    /// room-scoped; this is not a cross-room data grant.
    pub fn reference(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind) -> bool {
        let Some(idx) = index_obj(doc) else {
            return false;
        };
        let existing = match doc.get(&idx, id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        };
        if let Some(obj) = existing {
            // Refresh the type only if it actually differs, so
            // re-opening a record is not itself an edit.
            if str_at(doc, &obj, "kind").as_deref() != Some(kind.as_str()) {
                let _ = doc.put(&obj, "kind", kind.as_str());
            }
            false
        } else {
            let Ok(entry) = doc.put_object(&idx, id.as_str(), ObjType::Map) else {
                return false;
            };
            let _ = doc.put(&entry, "kind", kind.as_str());
            true
        }
    }

    /// Drop a record's reference from this room. The record itself is
    /// untouched — another room may hold an independently scoped replica.
    pub fn unreference(doc: &mut AutoCommit, id: &RecordId) -> bool {
        let Some(idx) = index_ro(doc) else {
            return false;
        };
        if matches!(doc.get(&idx, id.as_str()), Ok(Some(_))) {
            doc.delete(&idx, id.as_str()).is_ok()
        } else {
            false
        }
    }

    /// Whether this room references `id`.
    pub fn holds(doc: &AutoCommit, id: &RecordId) -> bool {
        index_ro(doc).is_some_and(|idx| matches!(doc.get(&idx, id.as_str()), Ok(Some(_))))
    }

    /// Every record this room references.
    pub fn references(doc: &AutoCommit) -> Vec<RecordCheckout> {
        let Some(idx) = index_ro(doc) else {
            return Vec::new();
        };
        let mut out: Vec<RecordCheckout> = doc
            .keys(&idx)
            .filter_map(|k| {
                let id = RecordId::parse(&k)?;
                let kind = match doc.get(&idx, k.as_str()) {
                    Ok(Some((Value::Object(ObjType::Map), obj))) => {
                        RecordKind::new(str_at(doc, &obj, "kind").unwrap_or_default())
                    }
                    _ => RecordKind::new(""),
                };
                Some(RecordCheckout::new(id, kind))
            })
            .collect();
        out.sort();
        out
    }

    /// Records this room holds that reference `target` under `relation` —
    /// how an app finds "the annotations for this document" without a
    /// second index.
    pub fn referencing(
        room: &AutoCommit,
        docs: &RecordSet,
        target: &RecordId,
        relation: &str,
    ) -> Vec<RecordCheckout> {
        references(room)
            .into_iter()
            .filter(|c| {
                docs.peek(&c.id).is_some_and(|d| {
                    super::references(d)
                        .iter()
                        .any(|r| &r.target == target && r.relation == relation)
                })
            })
            .collect()
    }

    /// Copy, the explicit verb: mints a new record id rather than reusing
    /// the origin's. The new document is created empty by the caller's
    /// next [`RecordSet::checkout`]; this returns the identity to check out.
    pub fn duplicate(doc: &mut AutoCommit, origin: &RecordCheckout) -> RecordCheckout {
        let copy = RecordCheckout::new(RecordId::minted(), origin.kind.clone());
        reference(doc, &copy.id, &copy.kind);
        copy
    }
}

impl PartialOrd for RecordCheckout {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RecordCheckout {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| self.kind.cmp(&other.kind))
    }
}

/// One record this process has open but is not currently showing.
struct Parked {
    /// `None` exactly while checked out — the live document is the
    /// toolkit's, not this one's.
    doc: Option<AutoCommit>,
    kind: RecordKind,
    /// Debounced writer for this record's file. `None` for an in-memory
    /// set, or when the store failed to open (reported once, not per frame).
    autosave: Option<crate::persist::Autosave>,
    /// Belongs to the record: an [`crate::state::UndoEntry`] is a pair of
    /// change hashes in *this* document's graph, so sharing a stack across
    /// records would revert the wrong history.
    history: crate::state::UndoLog,
}

/// The records one app process has open, and which is live. Owns
/// per-record persistence and parking — not collaboration: routing a
/// record's deltas is the toolkit's business (`ui::App` re-announces its
/// channel on checkout), since a channel is a property of the connection.
pub struct RecordSet {
    package_id: String,
    /// The room these records are being opened in — the cluster/workspace id.
    /// Stamped as a record's home when it is first created here.
    home: String,
    open: HashMap<RecordId, Parked>,
    active: Option<RecordId>,
    /// False for [`RecordSet::in_memory`]: no per-record files are opened, so
    /// a test drives the same checkout path without touching the disk.
    persistent: bool,
}

impl RecordSet {
    /// The production set: per-record documents under this package's state
    /// directory. `QOS_PACKAGE_ID` always wins over `manifest_id` (as in
    /// [`crate::persist::open`]) — resolving it differently here would
    /// split a package's records from its room index.
    pub fn open(manifest_id: impl Into<String>) -> Self {
        let package_id = std::env::var("QOS_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| manifest_id.into());
        let home = std::env::var("QOS_WORKSPACE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("QUICK_CLUSTER_ID")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            // A launcher-less process still gets an explicit, non-routable
            // scope.  It may not collaborate (the broker rejects nil), but
            // it must never fall back to the historical package-global file.
            .unwrap_or_else(|| uuid::Uuid::nil().to_string());
        Self {
            package_id,
            home,
            open: HashMap::new(),
            active: None,
            persistent: true,
        }
    }

    /// The package these records are stored under.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// A set that never touches the disk. For tests, for `--shots`, and for
    /// any run that must not leave state behind.
    pub fn in_memory() -> Self {
        Self {
            package_id: String::new(),
            home: String::new(),
            open: HashMap::new(),
            active: None,
            persistent: false,
        }
    }

    /// The room these records are opened in.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Name the room explicitly. Used by tests and by a host that opens
    /// records on behalf of a room other than its own launch environment.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<String>) -> Self {
        self.home = home.into();
        self
    }

    /// The record currently checked out, if any.
    pub fn active(&self) -> Option<&RecordId> {
        self.active.as_ref()
    }

    /// Every record this process has open, checked-out one included.
    pub fn open_records(&self) -> Vec<RecordCheckout> {
        let mut out: Vec<RecordCheckout> = self
            .open
            .iter()
            .map(|(id, p)| RecordCheckout::new(id.clone(), p.kind.clone()))
            .collect();
        out.sort();
        out
    }

    /// Read a parked record's document without checking it out. `None` for
    /// the record currently checked out — its document is the live one.
    pub fn peek(&self, id: &RecordId) -> Option<&AutoCommit> {
        self.open.get(id).and_then(|p| p.doc.as_ref())
    }

    /// Switch the live document to `want`: the outgoing document is parked
    /// and flushed, the incoming one loaded (or created) and referenced
    /// into `room`'s index. Returns whether anything changed — checking
    /// out the already-live record is a no-op, so a widget can declare its
    /// record every frame.
    pub fn checkout(
        &mut self,
        state: &mut crate::state::State,
        room: Option<&mut AutoCommit>,
        want: &RecordCheckout,
    ) -> bool {
        if self.active.as_ref() == Some(&want.id) {
            return false;
        }

        // A document with content and no record *becomes* this record's
        // rather than being replaced — this is "save untitled as
        // report.odt". Replacing it here would be the data loss records
        // exist to prevent.
        if self.active.is_none() && !state.doc.get_heads().is_empty() {
            let adopted = std::mem::replace(&mut state.doc, AutoCommit::new());
            state.adopt_doc(adopted);
            let autosave = self.persist_for(&want.id, &mut state.doc);
            let history = crate::state::UndoLog::new(&mut state.doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                },
            );
            return self.finish_checkout(state, room, want);
        }

        if let Some(prev) = self.active.take() {
            let outgoing = std::mem::replace(&mut state.doc, AutoCommit::new());
            if let Some(slot) = self.open.get_mut(&prev) {
                let mut outgoing = outgoing;
                if let Some(saver) = slot.autosave.as_mut() {
                    saver.request(&mut outgoing);
                }
                slot.doc = Some(outgoing);
            }
        }

        let incoming = if let Some(slot) = self.open.get_mut(&want.id) {
            slot.doc.take().unwrap_or_else(AutoCommit::new)
        } else {
            let (mut doc, autosave) = self.load(&want.id);
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                },
            );
            doc
        };

        state.adopt_doc(incoming);
        self.finish_checkout(state, room, want)
    }

    /// Stamps identity, applies declared references, and indexes the
    /// record (and its references) into the room — shared by both
    /// checkout routes.
    fn finish_checkout(
        &mut self,
        state: &mut crate::state::State,
        room: Option<&mut AutoCommit>,
        want: &RecordCheckout,
    ) -> bool {
        // Persistent records must always have an explicit room.  Failing
        // closed here prevents a launcher-less process from recreating the
        // old package-global record namespace.
        if self.persistent && self.home.is_empty() {
            tracing::error!(record = %want.id, "refusing to open a persistent record without a room scope");
            return false;
        }
        let scope = (!self.home.is_empty()).then_some(self.home.as_str());
        if !init_scoped(&mut state.doc, &want.id, &want.kind, &self.home, scope) {
            tracing::warn!(record = %want.id, room = %self.home, "record identity or room scope did not match stored document");
            return false;
        }
        for (target, relation) in &want.references {
            add_reference(
                &mut state.doc,
                &RecordRef::new(target.id.clone(), relation.clone()),
            );
        }
        if let Some(room) = room {
            room::reference(room, &want.id, &want.kind);
            // Targets too: a room holding the annotations but not their
            // target would hand a member a reference they can't reach.
            for (target, _) in &want.references {
                room::reference(room, &target.id, &target.kind);
            }
        }
        self.active = Some(want.id.clone());
        true
    }

    /// Open a record's stored document, and the writer for it.
    fn load(&self, id: &RecordId) -> (AutoCommit, Option<crate::persist::Autosave>) {
        let mut doc = AutoCommit::new();
        let autosave = self.persist_for(id, &mut doc);
        (doc, autosave)
    }

    /// Attach `doc` to this record's file: merge in what's stored, and
    /// hand back the writer. Merges rather than overwrites so adopted
    /// content survives. The writer is seeded from the **stored** heads,
    /// not merged — seeding from merged heads would mark adopted content
    /// as already saved.
    fn persist_for(&self, id: &RecordId, doc: &mut AutoCommit) -> Option<crate::persist::Autosave> {
        if !self.persistent {
            return None;
        }
        if self.home.is_empty() {
            return None;
        }
        let record_key = id.storage_key();
        let mut store = match app_state::DocStore::open_room_record(
            &self.package_id,
            &self.home,
            &record_key,
        ) {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "record store unavailable; this record runs without persistence"
                );
                return None;
            }
        };
        let stored = match store.load() {
            Ok(found) => found,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "stored record could not be read; continuing with what is live"
                );
                None
            }
        };
        // The old package-global record file is a read-only migration source.
        // It remains on disk until a later explicit cleanup, so a crash before
        // the first scoped autosave cannot lose it.
        let mut adopted_legacy = false;
        let stored = stored.or_else(|| {
            let legacy =
                app_state::DocStore::legacy_record_document_path(&self.package_id, &record_key)
                    .ok()?;
            if !legacy.is_file()
                || !app_state::DocStore::claim_legacy_record_migration(
                    &self.package_id,
                    &self.home,
                    &record_key,
                )
                .ok()?
            {
                return None;
            }
            let mut legacy_store =
                app_state::DocStore::open_record(&self.package_id, &record_key).ok()?;
            let legacy_doc = legacy_store.load().ok().flatten()?;
            tracing::info!(
                record = %id,
                room = %self.home,
                from = %legacy.display(),
                "adopting a legacy package-global record into a room"
            );
            adopted_legacy = true;
            Some(legacy_doc)
        });
        let mut autosave = crate::persist::Autosave::spawn(store, crate::persist::DEFAULT_DEBOUNCE);
        if let Some(mut stored) = stored {
            // Only seed heads that are already in this scoped store.  A
            // legacy document's heads are absent from the new file; seeding
            // them would append just the metadata change and strand its
            // dependencies after a crash.
            if !adopted_legacy {
                autosave.seed(&mut stored);
            }
            if let Err(e) = doc.merge(&mut stored) {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "stored record could not be merged; continuing with what is live"
                );
            }
        }
        Some(autosave)
    }

    // A host showing several records at once: none can be "checked out"
    // the way `checkout` assumes, since all are on screen in one frame.
    // These reach an open record where it lies; opening, persistence and
    // history are shared machinery.

    /// Open `want` if it is not open already, loading its document from
    /// its own file and bringing it into `room`'s index. Returns whether
    /// this opened it.
    pub fn open_record(&mut self, want: &RecordCheckout, room: Option<&mut AutoCommit>) -> bool {
        let fresh = !self.open.contains_key(&want.id);
        if fresh {
            let (mut doc, autosave) = self.load(&want.id);
            if self.persistent && self.home.is_empty() {
                tracing::error!(record = %want.id, "refusing to open a persistent record without a room scope");
                return false;
            }
            let scope = (!self.home.is_empty()).then_some(self.home.as_str());
            if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, scope) {
                tracing::warn!(record = %want.id, room = %self.home, "record identity or room scope did not match stored document");
                return false;
            }
            for (target, relation) in &want.references {
                add_reference(
                    &mut doc,
                    &RecordRef::new(target.id.clone(), relation.clone()),
                );
            }
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave,
                    history,
                },
            );
        }
        if let Some(room) = room {
            room::reference(room, &want.id, &want.kind);
            for (target, _) in &want.references {
                room::reference(room, &target.id, &target.kind);
            }
        }
        fresh
    }

    /// An open record's document, for a host showing several at once.
    /// `None` for a record not held here, or the one currently checked
    /// out (the live document is the toolkit's).
    pub fn document_mut(&mut self, id: &RecordId) -> Option<&mut AutoCommit> {
        self.open.get_mut(id).and_then(|p| p.doc.as_mut())
    }

    /// An open record's document together with its history, so a caller
    /// driving undo can do both against the same record.
    pub fn document_and_history_mut(
        &mut self,
        id: &RecordId,
    ) -> Option<(&mut AutoCommit, &mut crate::state::UndoLog)> {
        let parked = self.open.get_mut(id)?;
        let doc = parked.doc.as_mut()?;
        Some((doc, &mut parked.history))
    }

    /// Offer every open record's document to its own writer. The
    /// multi-record twin of [`Self::tick`], for a host with no single live
    /// document to hand in.
    pub fn tick_open(&mut self) {
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                saver.request(doc);
            }
        }
    }

    /// Write out every open record and wait for it to land. The
    /// multi-record twin of [`Self::flush`].
    pub fn flush_open(&mut self) {
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                saver.request(doc);
                saver.flush();
            }
        }
    }

    /// Offer the live document to the active record's writer. Cheap when
    /// nothing changed — a head comparison, no serialisation.
    pub fn tick(&mut self, live: &mut AutoCommit) {
        if let Some(slot) = self.active.as_ref().and_then(|id| self.open.get_mut(id))
            && let Some(saver) = slot.autosave.as_mut()
        {
            saver.request(live);
        }
    }

    /// Write out everything still pending, live document included, and wait
    /// for it to land.
    pub fn flush(&mut self, live: &mut AutoCommit) {
        if let Some(id) = self.active.clone()
            && let Some(slot) = self.open.get_mut(&id)
            && let Some(saver) = slot.autosave.as_mut()
        {
            saver.request(live);
            saver.flush();
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                saver.request(doc);
                saver.flush();
            }
        }
    }
}

/// A [`RecordSet`] several widgets share via a handle, so two tabs on the
/// same file stay one document rather than drifting under per-widget
/// ownership. Single-threaded on purpose — a widget tree is, and a
/// `Mutex` here would guard against nothing.
#[derive(Clone)]
pub struct RecordStore(std::rc::Rc<std::cell::RefCell<RecordSet>>);

impl RecordStore {
    /// Share an existing set.
    pub fn new(set: RecordSet) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(set)))
    }

    /// The production store: see [`RecordSet::open`].
    pub fn open(manifest_id: impl Into<String>) -> Self {
        Self::new(RecordSet::open(manifest_id))
    }

    /// A store that never touches disk: see [`RecordSet::in_memory`].
    pub fn in_memory() -> Self {
        Self::new(RecordSet::in_memory())
    }

    /// Open `want` if it is not open already. Returns whether this opened
    /// it.
    pub fn open_record(&self, want: &RecordCheckout) -> bool {
        self.0.borrow_mut().open_record(want, None)
    }

    /// Do something with a record's document. `None` if the record is not
    /// open here.
    pub fn with<R>(&self, id: &RecordId, f: impl FnOnce(&mut AutoCommit) -> R) -> Option<R> {
        let mut set = self.0.borrow_mut();
        set.document_mut(id).map(f)
    }

    /// Read a record's document.
    pub fn read<R>(&self, id: &RecordId, f: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        let mut set = self.0.borrow_mut();
        set.document_mut(id).map(|d| f(d))
    }

    /// Do something with a record's document *and* its history — commit an
    /// action, undo one.
    pub fn with_history<R>(
        &self,
        id: &RecordId,
        f: impl FnOnce(&mut AutoCommit, &mut crate::state::UndoLog) -> R,
    ) -> Option<R> {
        let mut set = self.0.borrow_mut();
        let (doc, history) = set.document_and_history_mut(id)?;
        Some(f(doc, history))
    }

    /// Every record this store holds.
    pub fn open_records(&self) -> Vec<RecordCheckout> {
        self.0.borrow().open_records()
    }

    /// Offer every open record to its writer. Once per idle tick.
    pub fn tick(&self) {
        self.0.borrow_mut().tick_open();
    }

    /// Write out every open record and wait for it to land.
    pub fn flush(&self) {
        self.0.borrow_mut().flush_open();
    }
}

impl std::fmt::Debug for RecordStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordStore")
            .field("open", &self.0.borrow().open.len())
            .finish()
    }
}

/// Run one record tick: if the widget tree names a record other than the
/// live one, park it and check the named one out. `ui::App` calls this
/// once per frame, before `sync`, so a widget that opened a file last
/// frame is editing that file's document by the time it next reads state.
/// The return value is the caller's cue to re-announce its collaboration
/// channel — a checkout is a change of address.
pub fn tick(
    set: &mut RecordSet,
    state: &mut crate::state::State,
    room: Option<&mut AutoCommit>,
    widget: &dyn crate::widget::Widget,
) -> Option<RecordId> {
    let want = widget.record_intent()?;
    if set.checkout(state, room, &want) {
        Some(want.id)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
        let _ = doc.put(&ROOT, key, value);
    }

    fn text(doc: &AutoCommit, key: &str) -> Option<String> {
        str_at(doc, &ROOT, key)
    }

    #[test]
    fn a_file_record_is_identified_by_its_path() {
        let id = RecordId::for_file("/work/report.odt");
        assert_eq!(id.as_str(), "file:/work/report.odt");
        assert_eq!(id.file_path().unwrap(), PathBuf::from("/work/report.odt"));
        // Same file, same record — that is the whole point of path identity.
        assert_eq!(id, RecordId::for_file("/work/./report.odt"));
        assert_eq!(id, RecordId::for_file("/work/drafts/../report.odt"));
    }

    #[test]
    fn minted_records_are_distinct() {
        assert_ne!(RecordId::minted(), RecordId::minted());
    }

    #[test]
    fn storage_keys_are_stable_safe_and_distinct() {
        let a = RecordId::for_file("/work/report.odt");
        let b = RecordId::for_file("/work/other.odt");
        assert_eq!(
            a.storage_key(),
            RecordId::for_file("/work/report.odt").storage_key()
        );
        assert_ne!(a.storage_key(), b.storage_key());
        // A traversal in the identity must not survive into the key.
        let key = RecordId::for_file("/work/../../etc/shadow").storage_key();
        assert!(!key.contains('/'), "key {key} must name one directory");
        assert!(!key.contains(".."), "key {key} must not traverse");

        // Paths that agree past the readable part of the key are still
        // two documents — a sanitise-only key would collide them.
        let deep_a =
            RecordId::for_file("/work/clients/northwind/2026/contracts/final/appendix-a.odt");
        let deep_b =
            RecordId::for_file("/work/clients/northwind/2026/contracts/final/appendix-b.odt");
        assert_ne!(
            deep_a.storage_key(),
            deep_b.storage_key(),
            "two long paths sharing a prefix must not share a file"
        );
    }

    #[test]
    fn a_record_carries_its_identity_type_and_home() {
        let mut doc = AutoCommit::new();
        let id = RecordId::for_file("/work/report.odt");
        let kind = RecordKind::new("document");
        init(&mut doc, &id, &kind, "room-1");
        let got = identity(&doc).expect("the document knows which record it is");
        assert_eq!(got.id, id);
        assert_eq!(got.kind, kind);
        assert_eq!(home(&doc).as_deref(), Some("room-1"));
    }

    #[test]
    fn re_opening_a_record_from_another_room_does_not_move_its_home() {
        let mut doc = AutoCommit::new();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");
        init(&mut doc, &id, &kind, "room-1");
        init(&mut doc, &id, &kind, "room-2");
        assert_eq!(
            home(&doc).as_deref(),
            Some("room-1"),
            "a record has one home and any number of references"
        );
    }

    #[test]
    fn annotations_reference_the_document_rather_than_living_in_it() {
        let mut markup = AutoCommit::new();
        let book = RecordId::for_file("/work/book.pdf");
        init(
            &mut markup,
            &RecordId::minted(),
            &RecordKind::new("annotation-set"),
            "room-1",
        );
        assert!(add_reference(
            &mut markup,
            &RecordRef::new(book.clone(), "marks-up")
        ));
        // Asserting the same reference twice is not two references.
        assert!(!add_reference(
            &mut markup,
            &RecordRef::new(book.clone(), "marks-up")
        ));
        let refs = references(&markup);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, book);
        assert_eq!(refs[0].relation, "marks-up");
    }

    #[test]
    fn a_room_holds_references_and_can_drop_them() {
        let mut room_doc = AutoCommit::new();
        let a = RecordId::for_file("/work/a.odt");
        let b = RecordId::for_file("/work/b.odt");
        let kind = RecordKind::new("document");
        assert!(room::reference(&mut room_doc, &a, &kind));
        assert!(
            !room::reference(&mut room_doc, &a, &kind),
            "referencing twice adds nothing"
        );
        assert!(room::reference(&mut room_doc, &b, &kind));
        assert!(room::holds(&room_doc, &a) && room::holds(&room_doc, &b));
        assert_eq!(room::references(&room_doc).len(), 2);

        assert!(room::unreference(&mut room_doc, &a));
        assert!(
            !room::holds(&room_doc, &a),
            "dropping the reference removes the access"
        );
        assert!(room::holds(&room_doc, &b), "and touches nothing else");
    }

    #[test]
    fn same_record_id_in_two_rooms_has_independent_in_memory_state() {
        // An id is an app-level name, not a capability.  Two room-scoped
        // sets must not accidentally turn a matching id into shared state.
        let mut room_a = AutoCommit::new();
        let mut room_b = AutoCommit::new();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");
        room::reference(&mut room_a, &id, &kind);
        room::reference(&mut room_b, &id, &kind);

        let mut set_a = RecordSet::in_memory().with_home("room-a");
        let mut state_a = State::new();
        let want = RecordCheckout::new(id.clone(), kind.clone());
        assert!(set_a.checkout(&mut state_a, Some(&mut room_a), &want));
        put_text(&mut state_a.doc, "title", "A only");

        let mut set_b = RecordSet::in_memory().with_home("room-b");
        let mut state_b = State::new();
        assert!(set_b.checkout(&mut state_b, Some(&mut room_b), &want));
        assert_eq!(text(&state_b.doc, "title"), None);
        put_text(&mut state_b.doc, "title", "B only");

        assert_eq!(
            text(&state_a.doc, "title").as_deref(),
            Some("A only"),
            "room B must not alias room A's record by id"
        );
    }

    #[test]
    fn duplicate_is_a_different_record() {
        let mut room_doc = AutoCommit::new();
        let origin = RecordCheckout::new(RecordId::minted(), RecordKind::new("book"));
        room::reference(&mut room_doc, &origin.id, &origin.kind);
        let copy = room::duplicate(&mut room_doc, &origin);
        assert_ne!(copy.id, origin.id, "copying mints a new identity");
        assert!(room::holds(&room_doc, &copy.id));
        assert!(room::holds(&room_doc, &origin.id));
    }

    #[test]
    fn checking_out_a_second_record_parks_the_first_intact() {
        // A second record must not clear the first — it's a different
        // document.
        let mut room_doc = AutoCommit::new();
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let kind = RecordKind::new("annotation-set");
        let a = RecordCheckout::new(RecordId::for_file("/work/a.pdf"), kind.clone());
        let b = RecordCheckout::new(RecordId::for_file("/work/b.pdf"), kind);

        set.checkout(&mut state, Some(&mut room_doc), &a);
        put_text(&mut state.doc, "note", "a's note");

        set.checkout(&mut state, Some(&mut room_doc), &b);
        assert_eq!(text(&state.doc, "note"), None, "b starts empty");
        put_text(&mut state.doc, "note", "b's note");

        assert_eq!(
            set.peek(&a.id).and_then(|d| text(d, "note")).as_deref(),
            Some("a's note"),
            "a's document is intact while b is live"
        );

        set.checkout(&mut state, Some(&mut room_doc), &a);
        assert_eq!(text(&state.doc, "note").as_deref(), Some("a's note"));
    }

    #[test]
    fn checking_out_the_live_record_changes_nothing() {
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
        assert!(set.checkout(&mut state, None, &want));
        put_text(&mut state.doc, "body", "typed");
        assert!(!set.checkout(&mut state, None, &want), "already live");
        assert_eq!(text(&state.doc, "body").as_deref(), Some("typed"));
    }

    /// A record adopted from a live document (the "save as" case) is
    /// written out rather than mistaken for something already on disk.
    #[test]
    // Shares a serial key with every other test mutating the process-global
    // `XDG_DATA_HOME` (see `persist::tests`), so they don't race on it.
    #[serial_test::serial(state_dir_env)]
    fn each_record_persists_to_its_own_file_and_comes_back() {
        let tmp = tempfile::tempdir().expect("temp dir");
        // SAFETY: this test is serialised on `record_state_dir`; no other
        // thread reads the variable between the set and the reads below.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        let room_id = "11111111-1111-4111-8111-111111111111";

        let a = RecordCheckout::new(
            RecordId::for_file("/work/a.odt"),
            RecordKind::new("document"),
        );
        let b = RecordCheckout::new(
            RecordId::for_file("/work/b.odt"),
            RecordKind::new("document"),
        );

        {
            let mut set = RecordSet::open("dev.qos.test-records").with_home(room_id);
            let mut state = State::new();
            let mut room = AutoCommit::new();
            // `a` is adopted: content exists before the record is named,
            // as when an app loads a file first.
            put_text(&mut state.doc, "body", "a's body");
            set.checkout(&mut state, Some(&mut room), &a);
            set.checkout(&mut state, Some(&mut room), &b);
            put_text(&mut state.doc, "body", "b's body");
            set.flush(&mut state.doc);
        }

        // A fresh process: same package, same record ids, nothing else shared.
        let mut set = RecordSet::open("dev.qos.test-records").with_home(room_id);
        let mut state = State::new();
        let mut room = AutoCommit::new();
        set.checkout(&mut state, Some(&mut room), &a);
        assert_eq!(
            text(&state.doc, "body").as_deref(),
            Some("a's body"),
            "the adopted record was written to its own file and came back"
        );
        set.checkout(&mut state, Some(&mut room), &b);
        assert_eq!(
            text(&state.doc, "body").as_deref(),
            Some("b's body"),
            "and the second record is its own file, not the first's"
        );

        let dir = tmp
            .path()
            .join("dev.qos.test-records")
            .join(app_state::ROOMS_DIR)
            .join(room_id)
            .join(app_state::RECORDS_DIR);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("records directory exists")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "one directory per record under {}, got {entries:?}",
            dir.display()
        );

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    #[serial_test::serial(state_dir_env)]
    fn same_record_id_in_two_rooms_never_aliases_and_each_hydrates_after_restart() {
        let tmp = tempfile::tempdir().expect("temp dir");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        let a = "22222222-2222-4222-8222-222222222222";
        let b = "33333333-3333-4333-8333-333333333333";
        let record = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

        for (room, body) in [(a, "private to a"), (b, "private to b")] {
            let mut set = RecordSet::open("dev.qos.scoped-records").with_home(room);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &record));
            put_text(&mut state.doc, "body", body);
            set.flush(&mut state.doc);
        }

        for (room, expected) in [(a, "private to a"), (b, "private to b")] {
            let mut set = RecordSet::open("dev.qos.scoped-records").with_home(room);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &record));
            assert_eq!(text(&state.doc, "body").as_deref(), Some(expected));
            assert_eq!(room_scope(&state.doc).as_deref(), Some(room));
        }

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn rejects_a_remote_document_stamped_for_another_room() {
        let id = RecordId::minted();
        let kind = RecordKind::new("note");
        let mut remote = AutoCommit::new();
        assert!(init_scoped(
            &mut remote,
            &id,
            &kind,
            "room-a",
            Some("room-a")
        ));
        put_text(&mut remote, "body", "remote merge");

        assert!(
            !init_scoped(&mut remote, &id, &kind, "room-b", Some("room-b")),
            "a received document's recorded room is authority, not a hint"
        );
        assert_eq!(room_scope(&remote).as_deref(), Some("room-a"));
    }

    #[test]
    fn a_records_annotations_are_found_through_the_reference() {
        let mut room_doc = AutoCommit::new();
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let file = RecordId::for_file("/work/book.pdf");
        room::reference(&mut room_doc, &file, &RecordKind::new("file"));

        let markup = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        set.checkout(&mut state, Some(&mut room_doc), &markup);
        add_reference(&mut state.doc, &RecordRef::new(file.clone(), "marks-up"));
        // Park it so `peek` can see it — the live document is not parked.
        let other = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        set.checkout(&mut state, Some(&mut room_doc), &other);

        let found = room::referencing(&room_doc, &set, &file, "marks-up");
        assert_eq!(found.len(), 1, "exactly the markup for this file");
        assert_eq!(found[0].id, markup.id);
    }
}
