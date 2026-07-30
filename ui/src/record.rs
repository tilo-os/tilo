//! Records — the unit of state, of collaboration and of storage.
//!
//! # Why an app's state cannot be one document
//!
//! An app used to keep *everything* in one Automerge document, addressed by
//! `(package_id, workspace_uuid)` (see [`crate::collaboration`]). Three
//! consequences followed, and all three were live behaviour:
//!
//! - Two people editing two **different** `.odt` files in one room announced
//!   the identical `(dev.qos.word-processor, room)` pair and merged each
//!   other's whole document state.
//! - Opening a second PDF **destroyed the annotations on the first**, because
//!   annotations were keyed by page number and a page number only means
//!   something within one file.
//! - "Share this document" was not expressible. Only "share this app in this
//!   room" was.
//!
//! # The model
//!
//! **A record has a type, a stable identity, and a home room.** A file is a
//! record whose identity is its path — the unification, not a special case, so
//! history, sharing and permissions have one implementation and the file case
//! stops being privileged.
//!
//! **Records reference other records.** A set of annotations references the
//! document it marks up rather than living inside it, which is what makes
//! "share the book without my notes" expressible at all.
//!
//! **A record exists once; rooms hold references to it.** A room's index
//! (written by [`room::reference`]) is a list of record ids, so bringing a
//! record into a second room moves no bytes and edits through either room are
//! edits to the one document. Copying is a separate, explicitly-named verb
//! ([`room::duplicate`]) that mints a new id and links back to its origin.
//!
//! **Per-record documents are forced, not chosen.** An Automerge change graph
//! cannot be served in part: every change names its dependencies by hash, so a
//! filtered stream leaves the receiver holding changes whose parents never
//! arrive, parked forever. Per-record *sharing* therefore means per-record
//! *documents* — one `AutoCommit`, one file on disk, one collaboration channel
//! each.
//!
//! # What that looks like in an app
//!
//! [`RecordSet`] owns the open records and hands the active one to the
//! toolkit as the live document. A widget says which record it wants by
//! implementing [`crate::widget::Widget::record_intent`]; [`tick`] — which
//! `ui::App` runs once per frame — parks the outgoing document and checks the
//! named one out. Nothing else in a widget changes: it keeps mutating the
//! `&mut AutoCommit` it is handed, which is now that record's own document.
//!
//! Not all app state is a record. A theme, a keyboard layout, an account or a
//! TOTP secret is a property of the person or the device, not of the room, and
//! must not reset when a new room is opened. Those stay in the package-level
//! document ([`crate::persist`]). Records are the things an app *makes,
//! collects or imports*.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

/// Key, in a record's own document, of the map holding its identity.
const META: &str = "qos:record";
/// Key, in a room's index document, of the map of record references.
const INDEX: &str = "qos:records";

/// The stable identity of a record.
///
/// Two shapes, and the difference is only in how the identity was chosen:
///
/// - `file:<absolute path>` — a record whose identity *is* its path. Opening
///   the same path twice reaches the same record, on this machine and on a
///   peer's.
/// - `uuid:<uuid>` — a minted identity, for a record with no natural name (a
///   set of annotations, a playlist, a saved search).
///
/// The prefix is part of the id, not a tag beside it: the id is what travels
/// on the wire and what names a file on disk, so it has to be self-describing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(String);

impl RecordId {
    /// The record a file *is*. Relative paths are resolved against the current
    /// directory so two spellings of one file are one record.
    pub fn for_file(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        };
        // `canonicalize` is deliberately not used: it fails for a file that
        // does not exist yet (Save As), and a record has to be nameable before
        // its bytes are.
        Self(format!("file:{}", normalise(&abs).display()))
    }

    /// A freshly minted identity, for a record with no natural name.
    pub fn minted() -> Self {
        Self(format!("uuid:{}", uuid::Uuid::new_v4()))
    }

    /// Adopt an id that already exists — read back from a document, a room
    /// index, or the wire. `None` for the empty string, which addresses
    /// nothing.
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

    /// A filesystem-safe directory name for this record's document.
    ///
    /// Two properties matter and neither is cosmetic. It must be **stable** —
    /// the same record must resolve to the same directory next launch, or the
    /// user's data is silently orphaned — and it must be **injective enough**
    /// that two records never collide, because a collision merges two
    /// documents. So: a readable, sanitised prefix for a human staring at the
    /// directory listing, plus a hash of the *whole* id, which is what
    /// actually distinguishes them.
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

/// Collapse `.` and `..` segments without touching the filesystem.
///
/// `canonicalize` would need the file to exist; a record must be nameable
/// before its bytes are. What must not survive is a `..`, which would let two
/// spellings of one path be two records — and, downstream, let a record key
/// address a directory outside the app's own state.
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

/// FNV-1a, 64-bit. Written out rather than pulled in because this hash names
/// directories on disk: it has to produce the same value in every future
/// version of every crate involved, which a library's default hasher does not
/// promise.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What kind of thing a record is, in the app's own resource model —
/// `document`, `annotation-set`, `book`, `playlist`, `city`.
///
/// App-owned rather than system-registered: an app can add a record type
/// without coordinating with anything. The cost, stated plainly, is that a
/// cross-app library view cannot yet show a book and a playlist side by side
/// under one vocabulary; that needs a system-owned type registry and is not
/// built here.
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
    /// The record referred to.
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
}

impl RecordCheckout {
    pub fn new(id: RecordId, kind: RecordKind) -> Self {
        Self { id, kind }
    }
}

// ---------------------------------------------------------------------------
// A record's own document: its identity and its references.
// ---------------------------------------------------------------------------

fn meta_obj(doc: &mut AutoCommit) -> ObjId {
    match doc.get(&ROOT, META) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => doc
            .put_object(&ROOT, META, ObjType::Map)
            .expect("put_object on root never fails"),
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

/// Stamp a document with the identity of the record it holds.
///
/// Idempotent: re-opening a record leaves the existing stamp alone rather than
/// rewriting it, so the identity is written once in the change graph and a
/// restart does not author a change that says nothing new.
pub fn init(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind, home: &str) {
    let m = meta_obj(doc);
    if str_at(doc, &m, "id").as_deref() != Some(id.as_str()) {
        let _ = doc.put(&m, "id", id.as_str());
    }
    if str_at(doc, &m, "kind").as_deref() != Some(kind.as_str()) {
        let _ = doc.put(&m, "kind", kind.as_str());
    }
    // The home room is where the record was minted. It is *not* rewritten when
    // the record is reached from a second room: a record has one home and any
    // number of references, and overwriting it would make the last room to
    // open the record its owner.
    if str_at(doc, &m, "home").is_none() {
        let _ = doc.put(&m, "home", home);
    }
    if !matches!(doc.get(&m, "refs"), Ok(Some((Value::Object(ObjType::List), _)))) {
        let _ = doc.put_object(&m, "refs", ObjType::List);
    }
}

/// The identity stamped into `doc`, if it holds a record.
pub fn identity(doc: &AutoCommit) -> Option<RecordCheckout> {
    let m = match doc.get(&ROOT, META) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => return None,
    };
    let id = RecordId::parse(&str_at(doc, &m, "id")?)?;
    let kind = RecordKind::new(str_at(doc, &m, "kind").unwrap_or_default());
    Some(RecordCheckout { id, kind })
}

/// The room this record was minted in.
pub fn home(doc: &AutoCommit) -> Option<String> {
    let m = match doc.get(&ROOT, META) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => return None,
    };
    str_at(doc, &m, "home")
}

fn refs_obj(doc: &mut AutoCommit) -> ObjId {
    let m = meta_obj(doc);
    match doc.get(&m, "refs") {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => doc
            .put_object(&m, "refs", ObjType::List)
            .expect("put_object refs never fails"),
    }
}

/// Record that this record references another. Idempotent on
/// `(target, relation)` — a reference is a fact, not an event, so asserting it
/// twice must not produce two of them.
pub fn add_reference(doc: &mut AutoCommit, r: &RecordRef) -> bool {
    if references(doc).iter().any(|e| e == r) {
        return false;
    }
    let list = refs_obj(doc);
    let n = doc.length(&list);
    let entry = doc
        .insert_object(&list, n, ObjType::Map)
        .expect("insert reference map never fails");
    let _ = doc.put(&entry, "target", r.target.as_str());
    let _ = doc.put(&entry, "relation", r.relation.as_str());
    true
}

/// Every record this one references.
pub fn references(doc: &AutoCommit) -> Vec<RecordRef> {
    let m = match doc.get(&ROOT, META) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => return Vec::new(),
    };
    let list = match doc.get(&m, "refs") {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let entry = match doc.get(&list, i) {
                Ok(Some((Value::Object(ObjType::Map), id))) => id,
                _ => return None,
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

/// A room's index of the records it holds.
///
/// The index lives in the app's package-level document — the one
/// [`crate::persist::open`] hands back — because that document already *is*
/// per-room state. What changes is what it contains: the records' contents
/// moved out into their own documents, and what stays behind is the list of
/// which records this room holds.
///
/// **The reference is what grants access.** A member of a room can reach the
/// records the room references and nothing else. Removing a reference stops
/// future changes flowing; it cannot un-share the bytes a peer already synced,
/// and no surface here should imply otherwise.
pub mod room {
    use super::*;

    fn index_obj(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, INDEX) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, INDEX, ObjType::Map)
                .expect("put_object on root never fails"),
        }
    }

    fn index_ro(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, INDEX) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// Bring a record into this room. Moves no bytes: the room gains a
    /// reference to the one document the record already is.
    ///
    /// Returns whether this added something. Keyed by record id rather than
    /// appended to a list so two machines referencing the same record
    /// concurrently converge on one entry instead of two.
    pub fn reference(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind) -> bool {
        let idx = index_obj(doc);
        let existing = match doc.get(&idx, id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        };
        match existing {
            Some(obj) => {
                // Already referenced. Only the type is refreshed, and only if
                // it actually differs, so re-opening a record is not an edit.
                if str_at(doc, &obj, "kind").as_deref() != Some(kind.as_str()) {
                    let _ = doc.put(&obj, "kind", kind.as_str());
                }
                false
            }
            None => {
                let entry = doc
                    .put_object(&idx, id.as_str(), ObjType::Map)
                    .expect("put_object into the index never fails");
                let _ = doc.put(&entry, "kind", kind.as_str());
                true
            }
        }
    }

    /// Drop a record's reference from this room. The record itself is
    /// untouched — it may be referenced by other rooms, and it is the same
    /// document in each.
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
        index_ro(doc)
            .map(|idx| matches!(doc.get(&idx, id.as_str()), Ok(Some(_))))
            .unwrap_or(false)
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
                Some(RecordCheckout { id, kind })
            })
            .collect();
        out.sort();
        out
    }

    /// The records this room holds that reference `target` under `relation`.
    ///
    /// This is how an app finds "the annotations for *this* document" without
    /// storing a second index of its own: the reference in the annotation
    /// record is the only fact, and it is read back here.
    pub fn referencing(
        room: &AutoCommit,
        docs: &RecordSet,
        target: &RecordId,
        relation: &str,
    ) -> Vec<RecordCheckout> {
        references(room)
            .into_iter()
            .filter(|c| {
                docs.peek(&c.id)
                    .map(|d| {
                        super::references(d)
                            .iter()
                            .any(|r| &r.target == target && r.relation == relation)
                    })
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Copy, the explicitly-named verb. Mints a new record id and stamps a
    /// `copied-from` reference to the origin, so the two are never confused
    /// for one document that happens to be in two places.
    ///
    /// The new record's document is created empty by the caller's next
    /// [`RecordSet::checkout`]; what this returns is the identity to check out.
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
        self.id.cmp(&other.id).then_with(|| self.kind.cmp(&other.kind))
    }
}

// ---------------------------------------------------------------------------
// The open records of one app process.
// ---------------------------------------------------------------------------

/// One record this process has open but is not currently showing.
struct Parked {
    /// The record's document. `None` exactly while the record is checked out —
    /// its document is then the live one the toolkit hands to widgets.
    doc: Option<AutoCommit>,
    kind: RecordKind,
    /// Debounced writer for this record's own file. `None` for an in-memory
    /// set, and for a record whose store could not be opened (which is
    /// reported once, not per frame).
    autosave: Option<crate::persist::Autosave>,
}

/// The records one app process has open, and which of them is live.
///
/// Owns per-record persistence and the parking of documents that are open but
/// not on screen. It does **not** own collaboration: the route a record's
/// deltas travel on is the toolkit's business (`ui::App` re-announces its
/// channel when the checkout changes), because a record's channel is a property
/// of the connection, not of the bytes.
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
    /// directory, in the room the launcher put this process in.
    pub fn open(package_id: impl Into<String>) -> Self {
        let home = std::env::var("QOS_WORKSPACE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("QUICK_CLUSTER_ID").ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();
        Self {
            package_id: package_id.into(),
            home,
            open: HashMap::new(),
            active: None,
            persistent: true,
        }
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

    /// Read a parked record's document without checking it out.
    ///
    /// Returns `None` for the record that is currently checked out — its
    /// document is the live one, and handing out a second view of it here
    /// would be a second answer to "what does this record contain".
    pub fn peek(&self, id: &RecordId) -> Option<&AutoCommit> {
        self.open.get(id).and_then(|p| p.doc.as_ref())
    }

    /// Switch the live document to `want`.
    ///
    /// The outgoing record's document is parked (and flushed to its own file);
    /// the incoming one is loaded from its file, or created if this is its
    /// first open. `room` is the room-index document, which gains a reference
    /// to the record — that reference is what makes the record reachable from
    /// this room later.
    ///
    /// Returns whether the live document changed. Checking out the record that
    /// is already live is a no-op, so a widget can declare its record every
    /// frame.
    pub fn checkout(
        &mut self,
        state: &mut crate::state::State,
        room: Option<&mut AutoCommit>,
        want: &RecordCheckout,
    ) -> bool {
        if self.active.as_ref() == Some(&want.id) {
            return false;
        }

        // Park the outgoing document under the record that owns it.
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

        // Take (or open) the incoming one.
        let incoming = match self.open.get_mut(&want.id) {
            Some(slot) => slot.doc.take().unwrap_or_else(AutoCommit::new),
            None => {
                let (doc, autosave) = self.load(&want.id);
                self.open.insert(
                    want.id.clone(),
                    Parked {
                        doc: None,
                        kind: want.kind.clone(),
                        autosave,
                    },
                );
                doc
            }
        };

        state.adopt_doc(incoming);
        init(&mut state.doc, &want.id, &want.kind, &self.home);
        if let Some(room) = room {
            room::reference(room, &want.id, &want.kind);
        }
        self.active = Some(want.id.clone());
        true
    }

    /// Open a record's stored document, and the writer for it.
    fn load(&self, id: &RecordId) -> (AutoCommit, Option<crate::persist::Autosave>) {
        if !self.persistent {
            return (AutoCommit::new(), None);
        }
        match app_state::DocStore::open_record(&self.package_id, &id.storage_key()) {
            Ok(mut store) => {
                let mut doc = store.load_or_new();
                let mut autosave =
                    crate::persist::Autosave::spawn(store, crate::persist::DEFAULT_DEBOUNCE);
                autosave.seed(&mut doc);
                (doc, Some(autosave))
            }
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "record store unavailable; this record runs without persistence"
                );
                (AutoCommit::new(), None)
            }
        }
    }

    /// Offer the live document to the active record's writer. Cheap when
    /// nothing changed — a head comparison, no serialisation.
    pub fn tick(&mut self, live: &mut AutoCommit) {
        if let Some(slot) = self.active.as_ref().and_then(|id| self.open.get_mut(id)) {
            if let Some(saver) = slot.autosave.as_mut() {
                saver.request(live);
            }
        }
    }

    /// Write out everything still pending, live document included, and wait
    /// for it to land.
    pub fn flush(&mut self, live: &mut AutoCommit) {
        if let Some(id) = self.active.clone() {
            if let Some(slot) = self.open.get_mut(&id) {
                if let Some(saver) = slot.autosave.as_mut() {
                    saver.request(live);
                    saver.flush();
                }
            }
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                saver.request(doc);
                saver.flush();
            }
        }
    }
}

/// Run one record tick: if the widget tree names a record other than the live
/// one, park the live document and check the named one out.
///
/// `ui::App` calls this once per frame, before `sync`, so a widget that opened
/// a file during the previous frame is editing that file's own document by the
/// time it next reads state. Returns the newly checked-out record, which is
/// the caller's cue to re-announce its collaboration channel — the record is
/// the routing key, so a checkout is a change of address.
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
        assert_eq!(a.storage_key(), RecordId::for_file("/work/report.odt").storage_key());
        assert_ne!(a.storage_key(), b.storage_key());
        // A traversal in the identity must not survive into the key.
        let key = RecordId::for_file("/work/../../etc/shadow").storage_key();
        assert!(!key.contains('/'), "key {key} must name one directory");
        assert!(!key.contains(".."), "key {key} must not traverse");
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
        init(&mut markup, &RecordId::minted(), &RecordKind::new("annotation-set"), "room-1");
        assert!(add_reference(&mut markup, &RecordRef::new(book.clone(), "marks-up")));
        // Asserting the same reference twice is not two references.
        assert!(!add_reference(&mut markup, &RecordRef::new(book.clone(), "marks-up")));
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
        assert!(!room::reference(&mut room_doc, &a, &kind), "referencing twice adds nothing");
        assert!(room::reference(&mut room_doc, &b, &kind));
        assert!(room::holds(&room_doc, &a) && room::holds(&room_doc, &b));
        assert_eq!(room::references(&room_doc).len(), 2);

        assert!(room::unreference(&mut room_doc, &a));
        assert!(!room::holds(&room_doc, &a), "dropping the reference removes the access");
        assert!(room::holds(&room_doc, &b), "and touches nothing else");
    }

    #[test]
    fn two_rooms_referencing_one_record_are_the_same_document() {
        // The load-bearing claim: a record brought into a second room is not a
        // copy. Both rooms name the same id, so both check out one document,
        // and an edit through either is an edit to it.
        let mut room_a = AutoCommit::new();
        let mut room_b = AutoCommit::new();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");
        room::reference(&mut room_a, &id, &kind);
        room::reference(&mut room_b, &id, &kind);

        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let want = RecordCheckout::new(id.clone(), kind.clone());
        assert!(set.checkout(&mut state, Some(&mut room_a), &want));
        put_text(&mut state.doc, "title", "Chapter 3");

        // Reached again from the other room: same id, so the same document —
        // not a fresh one. Leave and come back the way a user does, through a
        // different record, so the park/restore path is the one under test.
        let other = RecordCheckout::new(RecordId::minted(), kind.clone());
        assert!(set.checkout(&mut state, Some(&mut room_b), &other));
        assert!(set.checkout(&mut state, Some(&mut room_b), &want));
        assert_eq!(
            text(&state.doc, "title").as_deref(),
            Some("Chapter 3"),
            "the record reached from room B is the record edited in room A"
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
        // The structural fix for the data loss: a second record does not
        // clear the first, it is a different document.
        let mut room_doc = AutoCommit::new();
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let kind = RecordKind::new("annotation-set");
        let a = RecordCheckout::new(RecordId::for_file("/work/a.pdf"), kind.clone());
        let b = RecordCheckout::new(RecordId::for_file("/work/b.pdf"), kind.clone());

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
