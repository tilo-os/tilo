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

use std::collections::{BTreeMap, HashMap};

use crate::portable_storage::{PortableStorage, ProfileNamespace, StorageKey};
mod portable;
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

/// Key, in a record's own document, of the map holding its identity.
const META: &str = "tilo:record";
/// Deployed qOS metadata namespace. Existing documents retain this exact key.
const LEGACY_META: &str = "qos:record";
/// Key in a record document for the room that owns this local replica.
/// `RecordId` is a user/application name; it is not an authority boundary.
const ROOM: &str = "room";
/// Key, in a room's index document, of the map of record references.
const INDEX: &str = "tilo:records";
/// Deployed qOS room-index namespace. Existing indexes retain this exact key.
const LEGACY_INDEX: &str = "qos:records";

/// Upper bound for the records an app learns from one room index at once.
/// This is deliberately small: the index is replicated input, not a request
/// to allocate one local document per entry.
pub const MAX_DISCOVERED_ROOM_RECORDS: usize = 64;
const MAX_ROOM_INDEX_KEYS_SCANNED: usize = MAX_DISCOVERED_ROOM_RECORDS * 4;
const MAX_RECORD_ID_BYTES: usize = 1024;
const MAX_RECORD_KIND_BYTES: usize = 64;
const MAX_RECORD_RELATION_BYTES: usize = 64;

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
    /// Internal custody key for a package's existing room-primary document.
    /// It never travels as a record id on the collaboration wire.
    pub fn package_primary() -> Self {
        Self("@qos:package-primary".into())
    }
    pub fn is_package_primary(&self) -> bool {
        self.0 == "@qos:package-primary"
    }

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
    /// Invalid, oversized, or non-canonical names fail closed: IDs name
    /// collaboration routes and storage directories, so accepting arbitrary
    /// strings here would turn a room index into an allocation oracle.
    pub fn parse(s: &str) -> Option<Self> {
        valid_record_id(s, 0).then(|| Self(s.to_string()))
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

fn valid_record_id(s: &str, depth: usize) -> bool {
    if s.is_empty() || s.len() > MAX_RECORD_ID_BYTES || s.contains('\0') || depth > 4 {
        return false;
    }
    if let Some(path) = s.strip_prefix("file:") {
        let path = Path::new(path);
        return path.is_absolute() && normalise(path) == path;
    }
    if let Some(uuid) = s.strip_prefix("uuid:") {
        return uuid::Uuid::parse_str(uuid).is_ok();
    }
    if let Some(tab) = s.strip_prefix("tab:") {
        return tab.len() == 16 && tab.bytes().all(|c| c.is_ascii_hexdigit());
    }
    let Some(derived) = s.strip_prefix("derived:") else {
        return false;
    };
    let Some((role, target)) = derived.split_once('@') else {
        return false;
    };
    valid_label(role, MAX_RECORD_RELATION_BYTES)
        && depth
            .checked_add(1)
            .is_some_and(|next| valid_record_id(target, next))
}

fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().enumerate().all(|(i, byte)| {
            (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && (i != 0 || byte.is_ascii_alphabetic())
        })
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

    /// Whether this can safely travel in a room index or record stamp.
    pub fn is_valid(&self) -> bool {
        valid_label(&self.0, MAX_RECORD_KIND_BYTES)
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
    pub fn package_primary(kind: RecordKind) -> Self {
        Self::new(RecordId::package_primary(), kind)
    }

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

    /// Room indexes contain names supplied by collaborators.  Keep the
    /// transport boundary typed and bounded before a host considers one.
    pub fn is_valid(&self) -> bool {
        RecordId::parse(self.id.as_str()).is_some()
            && self.kind.is_valid()
            && self.valid_reference_shape()
    }

    fn is_valid_local_storage_request(&self) -> bool {
        (self.id.is_package_primary() || RecordId::parse(self.id.as_str()).is_some())
            && self.kind.is_valid()
            && self.valid_reference_shape()
    }

    fn valid_reference_shape(&self) -> bool {
        self.references.iter().all(|(target, relation)| {
            target.references.is_empty()
                && RecordId::parse(target.id.as_str()).is_some()
                && target.kind.is_valid()
                && valid_label(relation, MAX_RECORD_RELATION_BYTES)
        })
    }
}

/// Sealed evidence for one deployed local-only record spelling that predates
/// the self-describing record grammar. It can open only an existing qOS Notes
/// v1 canonical record; it never makes the spelling valid for rooms or wire
/// routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCanonicalCompatibility {
    profile: LocalCompatibilityProfile,
    checkout: RecordCheckout,
    workspace: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalCompatibilityProfile {
    QosNotesV1 { source_key: String },
}

impl LocalCanonicalCompatibility {
    /// Admit only the exact qOS Notes v1 source-derived record grammar.
    pub fn qos_notes_v1(workspace: &str, source_key: &str, record: &str) -> Option<Self> {
        if workspace.is_empty()
            || !lower_hex_64(source_key)
            || record != format!("derived:notes-legacy-v1@{source_key}")
        {
            return None;
        }
        Some(Self {
            profile: LocalCompatibilityProfile::QosNotesV1 {
                source_key: source_key.to_owned(),
            },
            checkout: RecordCheckout::new(
                RecordId(record.to_owned()),
                RecordKind::new("notes.notebook"),
            ),
            workspace: workspace.to_owned(),
        })
    }

    pub fn checkout(&self) -> &RecordCheckout {
        &self.checkout
    }
}

fn lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

enum ObjectAt {
    Missing,
    Object(ObjId),
    Invalid,
}

enum StringAt {
    Missing,
    String(String),
    Invalid,
}

fn object_at(doc: &AutoCommit, object: &ObjId, key: &str, kind: ObjType) -> ObjectAt {
    let Ok(mut values) = doc.get_all(object, key) else {
        return ObjectAt::Invalid;
    };
    if values.is_empty() {
        return ObjectAt::Missing;
    }
    if values.len() != 1 {
        return ObjectAt::Invalid;
    }
    match values.pop() {
        Some((Value::Object(found), id)) if found == kind => ObjectAt::Object(id),
        _ => ObjectAt::Invalid,
    }
}

fn selected_root_object(doc: &AutoCommit, current: &str, legacy: &str) -> ObjectAt {
    let current = object_at(doc, &ROOT, current, ObjType::Map);
    let legacy = object_at(doc, &ROOT, legacy, ObjType::Map);
    match (current, legacy) {
        (ObjectAt::Missing, ObjectAt::Missing) => ObjectAt::Missing,
        (ObjectAt::Object(object), ObjectAt::Missing)
        | (ObjectAt::Missing, ObjectAt::Object(object)) => ObjectAt::Object(object),
        _ => ObjectAt::Invalid,
    }
}

fn string_at(doc: &AutoCommit, object: &ObjId, key: &str) -> StringAt {
    let Ok(mut values) = doc.get_all(object, key) else {
        return StringAt::Invalid;
    };
    if values.is_empty() {
        return StringAt::Missing;
    }
    if values.len() != 1 {
        return StringAt::Invalid;
    }
    match values.pop() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(value) => StringAt::String(value.to_string()),
            _ => StringAt::Invalid,
        },
        _ => StringAt::Invalid,
    }
}

/// Select the document's one existing metadata namespace, or create the
/// current namespace only when neither frozen key exists.
fn meta_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    match selected_root_object(doc, META, LEGACY_META) {
        ObjectAt::Missing => doc.put_object(&ROOT, META, ObjType::Map).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

fn meta_ro(doc: &AutoCommit) -> Option<ObjId> {
    match selected_root_object(doc, META, LEGACY_META) {
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Missing | ObjectAt::Invalid => None,
    }
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match string_at(doc, obj, key) {
        StringAt::String(value) => Some(value),
        StringAt::Missing | StringAt::Invalid => None,
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
    // Primary custody is the checked package/room namespace, never a fake
    // record stamp. Keep this serialized internal id stable across hosts.
    if id.is_package_primary() {
        return primary_document(doc);
    }
    if RecordId::parse(id.as_str()).is_none() || !kind.is_valid() {
        return false;
    }
    let Some(m) = meta_obj(doc) else {
        // The document's root object is gone — nothing to stamp.
        return false;
    };
    let existing_id = string_at(doc, &m, "id");
    let existing_kind = string_at(doc, &m, "kind");
    let existing_home = string_at(doc, &m, "home");
    let existing_room = string_at(doc, &m, ROOM);
    let existing_refs = object_at(doc, &m, "refs", ObjType::List);
    if matches!(&existing_id, StringAt::String(existing) if existing != id.as_str())
        || matches!(&existing_id, StringAt::Invalid)
        || matches!(&existing_kind, StringAt::String(existing) if existing != kind.as_str())
        || matches!(&existing_kind, StringAt::Invalid)
        || matches!(&existing_home, StringAt::String(existing) if existing.is_empty())
        || matches!(&existing_home, StringAt::Invalid)
        || matches!(&existing_room, StringAt::Invalid)
        || room.is_some_and(
            |scope| matches!(&existing_room, StringAt::String(existing) if existing != scope),
        )
        || matches!(&existing_refs, ObjectAt::Invalid)
    {
        return false;
    }
    if matches!(&existing_id, StringAt::Missing) {
        let _ = doc.put(&m, "id", id.as_str());
    }
    if matches!(&existing_kind, StringAt::Missing) {
        let _ = doc.put(&m, "kind", kind.as_str());
    }
    // Home is the room that minted the record; never rewritten on a later
    // open, or the last room to open it would become the owner.
    if matches!(&existing_home, StringAt::Missing) {
        let _ = doc.put(&m, "home", home);
    }
    if let Some(room) = room
        && matches!(&existing_room, StringAt::Missing)
    {
        let _ = doc.put(&m, ROOM, room);
    }
    if matches!(&existing_refs, ObjectAt::Missing) {
        let _ = doc.put_object(&m, "refs", ObjType::List);
    }
    true
}

/// The identity stamped into `doc`, if it holds a record.
pub fn identity(doc: &AutoCommit) -> Option<RecordCheckout> {
    let m = meta_ro(doc)?;
    let id = RecordId::parse(&str_at(doc, &m, "id")?)?;
    let kind = RecordKind::new(str_at(doc, &m, "kind")?);
    let checkout = RecordCheckout::new(id, kind);
    checkout.is_valid().then_some(checkout)
}

/// The room this record was minted in.
pub fn home(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    str_at(doc, &m, "home")
}

/// The room this local record replica is bound to.  Absent only for records
/// written before room-partitioned persistence; callers may adopt those once
/// with [`init_scoped`], but must never treat an explicit different room as
/// interchangeable.
pub fn room_scope(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    str_at(doc, &m, ROOM)
}

/// `None` only if the containing document's root/meta object is gone; see
/// [`meta_obj`].
fn refs_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    let m = meta_obj(doc)?;
    match object_at(doc, &m, "refs", ObjType::List) {
        ObjectAt::Missing => doc.put_object(&m, "refs", ObjType::List).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

/// Idempotent on `(target, relation)`: a reference is a fact, not an
/// event.
pub fn add_reference(doc: &mut AutoCommit, r: &RecordRef) -> bool {
    if RecordId::parse(r.target.as_str()).is_none()
        || !valid_label(&r.relation, MAX_RECORD_RELATION_BYTES)
    {
        return false;
    }
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
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let ObjectAt::Object(list) = object_at(doc, &m, "refs", ObjType::List) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, i) else {
                return None;
            };
            let target = RecordId::parse(&str_at(doc, &entry, "target")?)?;
            let relation = str_at(doc, &entry, "relation")?;
            if !valid_label(&relation, MAX_RECORD_RELATION_BYTES) {
                return None;
            }
            Some(RecordRef { target, relation })
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
/// **The reference is what grants access.** A member can reach only the
/// records the room references. Removing a reference stops future changes
/// flowing; it cannot un-share bytes a peer already synced.
pub mod room {
    use super::{
        AutoCommit, INDEX, LEGACY_INDEX, ObjId, ObjType, ObjectAt, ROOT, ReadDoc, RecordCheckout,
        RecordId, RecordKind, RecordSet, StringAt, Transactable, object_at, selected_root_object,
        str_at, string_at,
    };

    /// Select one exact room-index namespace. Malformed, conflicted, or dual
    /// metadata fails closed; only an entirely absent index creates Tilo.
    fn index_obj(doc: &mut AutoCommit) -> Option<ObjId> {
        match selected_root_object(doc, INDEX, LEGACY_INDEX) {
            ObjectAt::Missing => doc.put_object(&ROOT, INDEX, ObjType::Map).ok(),
            ObjectAt::Object(object) => Some(object),
            ObjectAt::Invalid => None,
        }
    }

    fn index_ro(doc: &AutoCommit) -> Option<ObjId> {
        match selected_root_object(doc, INDEX, LEGACY_INDEX) {
            ObjectAt::Object(object) => Some(object),
            ObjectAt::Missing | ObjectAt::Invalid => None,
        }
    }

    /// Bring a record name into this room. Keyed by record id (not appended
    /// to a list) so two machines referencing the same name concurrently
    /// converge on one index entry. The document it resolves to remains
    /// room-scoped; this is not a cross-room data grant.
    pub fn reference(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind) -> bool {
        if RecordId::parse(id.as_str()).is_none() || !kind.is_valid() {
            return false;
        }
        let Some(idx) = index_obj(doc) else {
            return false;
        };
        match object_at(doc, &idx, id.as_str(), ObjType::Map) {
            ObjectAt::Object(object) => {
                // Refresh the type only if it actually differs, so
                // re-opening a record is not itself an edit.
                match string_at(doc, &object, "kind") {
                    StringAt::String(existing) if existing == kind.as_str() => {}
                    StringAt::String(_) => {
                        let _ = doc.put(&object, "kind", kind.as_str());
                    }
                    StringAt::Missing | StringAt::Invalid => return false,
                }
                false
            }
            ObjectAt::Missing => {
                let Ok(entry) = doc.put_object(&idx, id.as_str(), ObjType::Map) else {
                    return false;
                };
                let _ = doc.put(&entry, "kind", kind.as_str());
                true
            }
            ObjectAt::Invalid => false,
        }
    }

    /// Drop a record's reference from this room. The record itself is
    /// untouched — another room may hold an independently scoped replica.
    pub fn unreference(doc: &mut AutoCommit, id: &RecordId) -> bool {
        let Some(idx) = index_ro(doc) else {
            return false;
        };
        match object_at(doc, &idx, id.as_str(), ObjType::Map) {
            ObjectAt::Object(_) => doc.delete(&idx, id.as_str()).is_ok(),
            ObjectAt::Missing | ObjectAt::Invalid => false,
        }
    }

    /// Whether this room references `id`.
    pub fn holds(doc: &AutoCommit, id: &RecordId) -> bool {
        index_ro(doc).is_some_and(|idx| {
            let ObjectAt::Object(entry) = object_at(doc, &idx, id.as_str(), ObjType::Map) else {
                return false;
            };
            str_at(doc, &entry, "kind")
                .map(RecordKind::new)
                .is_some_and(|kind| kind.is_valid())
        })
    }

    /// Every well-formed record this room references.  This is retained for
    /// callers that already have their own work bound; hosts that learn a
    /// room supplied index should use [`discoverable_references`] instead.
    pub fn references(doc: &AutoCommit) -> Vec<RecordCheckout> {
        let Some(idx) = index_ro(doc) else {
            return Vec::new();
        };
        let mut out: Vec<RecordCheckout> = doc
            .keys(&idx)
            .filter_map(|k| {
                let id = RecordId::parse(&k)?;
                let ObjectAt::Object(object) = object_at(doc, &idx, k.as_str(), ObjType::Map)
                else {
                    return None;
                };
                let kind = RecordKind::new(str_at(doc, &object, "kind")?);
                let checkout = RecordCheckout::new(id, kind);
                checkout.is_valid().then_some(checkout)
            })
            .collect();
        out.sort();
        out
    }

    /// A deterministic, resource-bounded view of collaborator-supplied room
    /// names.  It reads at most a small fixed prefix of the CRDT map and
    /// returns at most [`super::MAX_DISCOVERED_ROOM_RECORDS`] valid entries.
    /// It does **not** open records, create persistence, or join record
    /// routes; the caller must make that explicit after choosing a record.
    pub fn discoverable_references(doc: &AutoCommit) -> Vec<RecordCheckout> {
        let Some(idx) = index_ro(doc) else {
            return Vec::new();
        };
        let mut out: Vec<RecordCheckout> = doc
            .keys(&idx)
            .take(super::MAX_ROOM_INDEX_KEYS_SCANNED)
            .filter_map(|key| {
                let id = RecordId::parse(&key)?;
                let ObjectAt::Object(entry) = object_at(doc, &idx, key.as_str(), ObjType::Map)
                else {
                    return None;
                };
                let kind = RecordKind::new(str_at(doc, &entry, "kind")?);
                let checkout = RecordCheckout::new(id, kind);
                checkout.is_valid().then_some(checkout)
            })
            .collect();
        out.sort();
        out.truncate(super::MAX_DISCOVERED_ROOM_RECORDS);
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
    action_state: crate::bound_dispatch::RecordActionState,
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
    /// Explicitly admitted local-only records whose deployed spelling is not
    /// valid for room discovery or collaboration routing.
    local_compatibility: HashMap<RecordId, LocalCanonicalCompatibility>,
    /// Names currently granted by the room index.  Discovery is intentionally
    /// separate from `open`: a hostile or merely large index must not create
    /// local replicas or subscribe the app to unrelated documents.
    known: BTreeMap<RecordId, RecordKind>,
    active: Option<RecordId>,
    /// False for [`RecordSet::in_memory`]: no per-record files are opened, so
    /// a test drives the same checkout path without touching the disk.
    persistent: bool,
    canonical_root: Option<PathBuf>,
    portable: Option<PortableStorage>,
    pending: HashMap<RecordId, Reserved>,
    next_reservation: u64,
    // Drop after the writers and reservations above. Temporary profiles must
    // outlive asynchronous flushes and recovery handles, not just their host.
    _profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

struct Reserved {
    generation: u64,
    store: Option<std::sync::Arc<app_state::DocStore>>,
    actor: Option<automerge::ActorId>,
}

impl RecordSet {
    /// The production set: per-record documents under this package's state
    /// directory. `TILO_PACKAGE_ID` always wins over `manifest_id` (as in
    /// [`crate::persist::open`]) — resolving it differently here would
    /// split a package's records from its room index.
    pub fn open(manifest_id: impl Into<String>) -> Self {
        let package_id = std::env::var("TILO_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| manifest_id.into());
        let home = std::env::var("TILO_WORKSPACE_ID")
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
            local_compatibility: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            canonical_root: None,
            portable: None,
            pending: HashMap::new(),
            next_reservation: 0,
            _profile_lifetime: None,
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
            local_compatibility: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: false,
            canonical_root: None,
            portable: None,
            pending: HashMap::new(),
            next_reservation: 0,
            _profile_lifetime: None,
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

    /// The bounded, validated names currently granted by the room index.
    /// Unlike [`Self::open_records`], this does not imply their documents
    /// were loaded, persisted, or joined on the collaboration transport.
    pub fn known_records(&self) -> Vec<RecordCheckout> {
        self.known
            .iter()
            .map(|(id, kind)| RecordCheckout::new(id.clone(), kind.clone()))
            .collect()
    }

    /// Refresh the room-granted record names without opening any of them.
    /// A removed/tombstoned index entry disappears from future discovery, but
    /// already-open data remains in memory: revocation stops future access;
    /// it cannot un-share bytes the process has already received.
    pub fn discover_room(&mut self, room: &AutoCommit) -> Vec<RecordCheckout> {
        let found = room::discoverable_references(room);
        self.known = found
            .iter()
            .map(|checkout| (checkout.id.clone(), checkout.kind.clone()))
            .collect();
        found
    }

    fn remember(&mut self, want: &RecordCheckout) {
        if want.is_valid() {
            self.known.insert(want.id.clone(), want.kind.clone());
            for (target, _) in &want.references {
                if target.is_valid() {
                    self.known.insert(target.id.clone(), target.kind.clone());
                }
            }
        }
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
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record checkout");
            return false;
        }
        if self.canonical_root.is_some() || self.portable.is_some() {
            return false;
        }
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
            let Ok(autosave) = self.persist_for(want, &mut state.doc) else {
                return false;
            };
            let history = crate::state::UndoLog::new(&mut state.doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: crate::bound_dispatch::RecordActionState::default(),
                },
            );
            return self.finish_checkout(state, room, want);
        }

        // A malformed incoming legacy document must fail before custody of
        // the valid active document moves into a parked slot.
        let prepared = if self.open.contains_key(&want.id) {
            None
        } else {
            let Ok(prepared) = self.load(want) else {
                return false;
            };
            Some(prepared)
        };

        if let Some(prev) = self.active.take() {
            let outgoing = std::mem::replace(&mut state.doc, AutoCommit::new());
            if let Some(slot) = self.open.get_mut(&prev) {
                let mut outgoing = outgoing;
                if let Some(saver) = slot.autosave.as_mut() {
                    slot.action_state.commit_for_save(&mut outgoing);
                    saver.request(&mut outgoing);
                }
                slot.doc = Some(outgoing);
            }
        }

        let incoming = if let Some(slot) = self.open.get_mut(&want.id) {
            slot.doc.take().unwrap_or_else(AutoCommit::new)
        } else {
            let (mut doc, autosave) = prepared.expect("unopened record was prepared");
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: crate::bound_dispatch::RecordActionState::default(),
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
        self.remember(want);
        true
    }

    /// Open a record's stored document, and the writer for it.
    fn load(
        &self,
        want: &RecordCheckout,
    ) -> Result<(AutoCommit, Option<crate::persist::Autosave>), RecordOpenError> {
        if self.persistent && self.home.is_empty() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let mut doc = AutoCommit::new();
        let autosave = self.persist_for(want, &mut doc)?;
        if self.persistent
            && !init_scoped(&mut doc, &want.id, &want.kind, &self.home, Some(&self.home))
        {
            return Err(RecordOpenError::IdentityMismatch);
        }
        Ok((doc, autosave))
    }

    /// Attach `doc` to this record's file: merge in what's stored, and
    /// hand back the writer. Merges rather than overwrites so adopted
    /// content survives. The writer is seeded from the **stored** heads,
    /// not merged — seeding from merged heads would mark adopted content
    /// as already saved.
    fn persist_for(
        &self,
        want: &RecordCheckout,
        doc: &mut AutoCommit,
    ) -> Result<Option<crate::persist::Autosave>, RecordOpenError> {
        let id = &want.id;
        if !self.persistent {
            return Ok(None);
        }
        if self.home.is_empty() {
            return Ok(None);
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
                    "record store unavailable; writes refused"
                );
                return Ok(Some(crate::persist::Autosave::disabled(format!(
                    "record {id} store unavailable: {e}"
                ))));
            }
        };
        let stored = match store.load() {
            Ok(found) => found,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "stored record could not be read; original bytes retained and writes refused"
                );
                return Ok(Some(crate::persist::Autosave::disabled(format!(
                    "stored record {id} could not be read: {e}"
                ))));
            }
        };
        // The old package-global record file is a read-only migration source.
        // It remains on disk until a later explicit cleanup, so a crash before
        // the first scoped autosave cannot lose it.
        let mut adopted_legacy = false;
        let stored = if stored.is_some() {
            stored
        } else {
            let legacy =
                app_state::DocStore::legacy_record_document_path(&self.package_id, &record_key)?;
            if !legacy.is_file() {
                None
            } else {
                let mut legacy_store =
                    app_state::DocStore::open_record(&self.package_id, &record_key)?;
                let mut legacy_doc = legacy_store.load()?.ok_or_else(|| {
                    app_state::StoreError::EmptyExistingDocument {
                        path: legacy.clone(),
                    }
                })?;
                if identity(&legacy_doc)
                    .is_none_or(|found| found.id != want.id || found.kind != want.kind)
                    || home(&legacy_doc).as_deref() != Some(self.home.as_str())
                    || room_scope(&legacy_doc).is_some_and(|scope| scope != self.home)
                    || !init_scoped(
                        &mut legacy_doc,
                        &want.id,
                        &want.kind,
                        &self.home,
                        Some(&self.home),
                    )
                    || !app_state::DocStore::claim_legacy_record_migration(
                        &self.package_id,
                        &self.home,
                        &record_key,
                    )?
                {
                    return Err(RecordOpenError::IdentityMismatch);
                }
                tracing::info!(
                    record = %id,
                    room = %self.home,
                    from = %legacy.display(),
                    "adopting a legacy package-global record into a room"
                );
                adopted_legacy = true;
                Some(legacy_doc)
            }
        };
        // Merge transactionally. Automerge may have applied part of a failed
        // merge; never publish or enqueue that partial result to the writer.
        let mut merged = doc.clone();
        let mut stored = stored;
        if let Some(stored) = stored.as_mut()
            && let Err(error) = merged.merge(stored)
        {
            tracing::error!(record = %id, %error, "record merge refused; original stored bytes retained");
            return Ok(Some(crate::persist::Autosave::disabled(format!(
                "stored record {id} could not be merged: {error}"
            ))));
        }
        if !init_scoped(
            &mut merged,
            &want.id,
            &want.kind,
            &self.home,
            Some(&self.home),
        ) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        // The package actor belongs to its index, and a bootstrap fork actor
        // belongs to pre-admission edits. All subsequent canonical record
        // operations belong to this record's original persistent writer.
        merged.set_actor(store.actor().clone());
        *doc = merged;
        let mut autosave = crate::persist::Autosave::spawn(store, crate::persist::DEFAULT_DEBOUNCE);
        if let Some(mut stored) = stored {
            // Only seed heads that are already in this scoped store.  A
            // legacy document's heads are absent from the new file; seeding
            // them would append just the metadata change and strand its
            // dependencies after a crash.
            if !adopted_legacy {
                autosave.seed(&mut stored);
            }
        }
        Ok(Some(autosave))
    }

    // A host showing several records at once: none can be "checked out"
    // the way `checkout` assumes, since all are on screen in one frame.
    // These reach an open record where it lies; opening, persistence and
    // history are shared machinery.

    /// Open `want` if it is not open already, loading its document from
    /// its own file and bringing it into `room`'s index. Returns whether
    /// this opened it.
    pub fn open_record(&mut self, want: &RecordCheckout, room: Option<&mut AutoCommit>) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record open");
            return false;
        }
        if self.canonical_root.is_some() || self.portable.is_some() {
            return matches!(self.try_open_record(want), Ok(RecordOpenOutcome::Loaded));
        }
        let fresh = !self.open.contains_key(&want.id);
        if fresh {
            let Ok((mut doc, autosave)) = self.load(want) else {
                return false;
            };
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
                    action_state: crate::bound_dispatch::RecordActionState::default(),
                },
            );
        }
        if let Some(room) = room {
            room::reference(room, &want.id, &want.kind);
            for (target, _) in &want.references {
                room::reference(room, &target.id, &target.kind);
            }
        }
        self.remember(want);
        fresh
    }

    fn canonical_directory(&self, want: &RecordCheckout) -> Result<PathBuf, RecordOpenError> {
        let root = self
            .canonical_root
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        if want.id.is_package_primary() {
            return Ok(app_state::DocStore::room_primary_dir_at(
                root,
                &self.package_id,
                &self.home,
            )?);
        }
        Ok(app_state::DocStore::room_record_dir_at(
            root,
            &self.package_id,
            &self.home,
            &want.id.storage_key(),
        )?)
    }

    fn try_open_record(
        &mut self,
        want: &RecordCheckout,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        self.try_open_record_with_recovery(want, true)
    }

    fn try_open_local_compatibility(
        &mut self,
        evidence: &LocalCanonicalCompatibility,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        if self.portable.is_some()
            || self.package_id != "dev.qos.notes"
            || self.home != evidence.workspace
            || self.home.is_empty()
        {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let id = &evidence.checkout.id;
        if self.pending.contains_key(id) {
            return Err(RecordOpenError::Busy);
        }
        if let Some(slot) = self.open.get(id) {
            if slot.kind != evidence.checkout.kind
                || self.local_compatibility.get(id) != Some(evidence)
            {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
            validate_local_compatibility(doc, evidence, &self.package_id, &self.home)?;
            return Ok(RecordOpenOutcome::AlreadyOpen);
        }
        if self.local_compatibility.contains_key(id) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let root = self
            .canonical_root
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        let directory = app_state::DocStore::room_record_dir_at(
            root,
            &self.package_id,
            &self.home,
            &id.storage_key(),
        )?;
        if !directory.is_dir()
            || !directory.join(app_state::ACTOR_FILE).is_file()
            || !directory.join(app_state::DOCUMENT_FILE).is_file()
        {
            return Err(RecordOpenError::MissingRecord);
        }
        let mut store = app_state::DocStore::open_canonical_at(&self.package_id, &directory)?;
        let loaded = store
            .load_canonical_complete()?
            .ok_or(RecordOpenError::MissingRecord)?;
        store.validate_canonical_schema(loaded.document())?;
        validate_local_compatibility(loaded.document(), evidence, &self.package_id, &self.home)?;
        let mut doc = store.accept_canonical(loaded)?;
        let owner = std::sync::Arc::new(store);
        let autosave =
            app_state::Autosave::try_spawn_canonical(&owner, app_state::DEFAULT_DEBOUNCE)?;
        let history = crate::state::UndoLog::new(&mut doc);
        self.open.insert(
            id.clone(),
            Parked {
                doc: Some(doc),
                kind: evidence.checkout.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: crate::bound_dispatch::RecordActionState::default(),
            },
        );
        self.local_compatibility
            .insert(id.clone(), evidence.clone());
        Ok(RecordOpenOutcome::Loaded)
    }

    fn try_open_record_with_recovery(
        &mut self,
        want: &RecordCheckout,
        recover_partial: bool,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        if self.portable.is_some() {
            return self.open_portable(want);
        }
        let directory = self.canonical_directory(want)?;
        if self.pending.contains_key(&want.id) {
            return Err(RecordOpenError::Busy);
        }
        if let Some(slot) = self.open.get(&want.id) {
            if slot.kind != want.kind {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
            validate_identity(doc, want, &self.home)?;
            return Ok(RecordOpenOutcome::AlreadyOpen);
        }
        let mut store = app_state::DocStore::open_canonical_at(&self.package_id, &directory)?;
        let loaded = if recover_partial {
            store.load_canonical()?
        } else {
            store.load_canonical_complete()?
        }
        .ok_or(RecordOpenError::MissingRecord)?;
        validate_identity(loaded.document(), want, &self.home)?;
        let mut doc = store.accept_canonical(loaded)?;
        let owner = std::sync::Arc::new(store);
        let autosave =
            app_state::Autosave::try_spawn_canonical(&owner, app_state::DEFAULT_DEBOUNCE)?;
        let history = crate::state::UndoLog::new(&mut doc);
        self.open.insert(
            want.id.clone(),
            Parked {
                doc: Some(doc),
                kind: want.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: crate::bound_dispatch::RecordActionState::default(),
            },
        );
        Ok(RecordOpenOutcome::Loaded)
    }

    /// An open record's document, for a host showing several at once.
    /// `None` for a record not held here, or the one currently checked
    /// out (the live document is the toolkit's).
    pub fn document_mut(&mut self, id: &RecordId) -> Option<&mut AutoCommit> {
        let parked = self.open.get_mut(id)?;
        if let Some(doc) = parked.doc.as_mut() {
            parked
                .action_state
                .finish_external(doc, &mut parked.history);
        }
        parked.action_state.generation = parked
            .action_state
            .generation
            .and_then(|generation| generation.checked_add(1));
        parked.doc.as_mut()
    }

    /// An open record's document together with its history, so a caller
    /// driving undo can do both against the same record.
    pub fn document_and_history_mut(
        &mut self,
        id: &RecordId,
    ) -> Option<(&mut AutoCommit, &mut crate::state::UndoLog)> {
        let parked = self.open.get_mut(id)?;
        if let Some(doc) = parked.doc.as_mut() {
            parked
                .action_state
                .finish_external(doc, &mut parked.history);
        }
        parked.action_state.generation = parked
            .action_state
            .generation
            .and_then(|generation| generation.checked_add(1));
        let doc = parked.doc.as_mut()?;
        Some((doc, &mut parked.history))
    }

    /// Offer every open record's document to its own writer. The
    /// multi-record twin of [`Self::tick`], for a host with no single live
    /// document to hand in.
    pub fn tick_open(&mut self) {
        if self.portable.is_some() {
            let _ = self.flush_portable(None);
            return;
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
            }
        }
    }

    /// Write out every open record and wait for it to land. The
    /// multi-record twin of [`Self::flush`].
    pub fn flush_open(&mut self) {
        if self.portable.is_some() {
            let _ = self.flush_portable(None);
            return;
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
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
            slot.action_state.commit_for_save(live);
            saver.request(live);
        }
    }

    /// Flush active and parked records, reporting unavailable or failed writers.
    ///
    /// # Errors
    /// Returns record-specific persistence failures without discarding live bytes.
    pub fn try_flush(&mut self, live: &mut AutoCommit) -> Result<(), String> {
        self.try_flush_documents(Some(live))
    }

    fn try_flush_documents(&mut self, mut live: Option<&mut AutoCommit>) -> Result<(), String> {
        let canonical = self.canonical_root.is_some();
        let mut errors = Vec::new();
        for (id, slot) in &mut self.open {
            let Some(saver) = slot.autosave.as_mut() else {
                if self.persistent {
                    errors.push(format!("{id}: record persistence unavailable"));
                }
                continue;
            };
            let doc = if self.active.as_ref() == Some(id) {
                live.as_deref_mut()
            } else {
                slot.doc.as_mut()
            };
            let Some(doc) = doc else {
                errors.push(format!("{id}: record document unavailable for flush"));
                continue;
            };
            if canonical && saver.last_error().is_some() {
                if let Err(error) = saver.try_restart_canonical() {
                    errors.push(format!("{id}: {error}"));
                    continue;
                }
                slot.action_state.commit_for_save(doc);
                saver.request_checkpoint(doc);
            } else {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
            }
            if let Err(error) = saver.try_flush() {
                errors.push(format!("{id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Write out everything still pending, live document included, and wait
    /// for it to land.
    pub fn flush(&mut self, live: &mut AutoCommit) {
        if let Err(error) = self.try_flush(live) {
            tracing::error!(%error, "record persistence failed");
        }
    }
}

/// A [`RecordSet`] several widgets share via a handle, so two tabs on the
/// same file stay one document rather than drifting under per-widget
/// ownership. Single-threaded on purpose — a widget tree is, and a
/// `Mutex` here would guard against nothing.
#[derive(Clone)]
pub struct RecordStore(std::rc::Rc<std::cell::RefCell<RecordSet>>);

/// Explicit profile-scoped owner registry. Weak entries never keep a closed writer alive.
pub struct RecordOwners {
    root: PathBuf,
    portable: Option<PortableStorage>,
    owners: HashMap<(String, String), std::rc::Weak<std::cell::RefCell<RecordSet>>>,
    profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

impl RecordOwners {
    pub fn new(profile_state_root: PathBuf) -> Self {
        Self {
            root: profile_state_root,
            portable: None,
            owners: HashMap::new(),
            profile_lifetime: None,
        }
    }

    /// Retain a host-owned profile resource until its last writer/recovery owner
    /// is gone. Construct this before opening any records.
    pub(crate) fn with_profile_lifetime(
        profile_state_root: PathBuf,
        lifetime: std::rc::Rc<dyn std::any::Any>,
    ) -> Self {
        Self {
            profile_lifetime: Some(lifetime),
            ..Self::new(profile_state_root)
        }
    }

    /// Get the unique live owner for this package and workspace.
    ///
    /// # Errors
    /// Rejects an invalid explicit root, package or workspace.
    pub fn open(
        &mut self,
        package_id: &str,
        workspace_id: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        if self.portable.is_some() {
            app_state::validate_record_namespace(package_id, workspace_id)?;
        } else {
            app_state::DocStore::room_record_dir_at(
                &self.root,
                package_id,
                workspace_id,
                "validation",
            )?;
        }

        if let Some(cache) = &self.portable
            && let Some(owner) = cache.record_owner(package_id, workspace_id)
        {
            return Ok(RecordStore(owner));
        }
        let key = (package_id.to_owned(), workspace_id.to_owned());
        if let Some(owner) = self.owners.get(&key).and_then(std::rc::Weak::upgrade) {
            return Ok(RecordStore(owner));
        }
        let store = RecordStore::new(RecordSet {
            package_id: package_id.to_owned(),
            home: workspace_id.to_owned(),
            open: HashMap::new(),
            local_compatibility: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            canonical_root: self.portable.is_none().then(|| self.root.clone()),
            portable: self.portable.clone(),
            pending: HashMap::new(),
            next_reservation: 0,
            _profile_lifetime: self.profile_lifetime.clone(),
        });
        if let Some(cache) = &self.portable {
            cache.register_record_owner(package_id, workspace_id, &store.0);
        }
        self.owners.insert(key, std::rc::Rc::downgrade(&store.0));
        Ok(store)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordSaveStatus {
    Saved,
    Saving,
    Failed(String),
    MemoryOnly,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordOpenOutcome {
    Loaded,
    AlreadyOpen,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordOpenError {
    #[error(transparent)]
    Store(#[from] app_state::StoreError),
    #[error("record identity does not match its binding")]
    IdentityMismatch,
    #[error("record does not exist")]
    MissingRecord,
    #[error("record is reserved or already initialized")]
    Busy,
    #[error("the new record identity already exists or is reserved")]
    AlreadyExists,
    #[error("record initialization failed: {0}")]
    Initialization(String),
}

fn primary_document(doc: &AutoCommit) -> bool {
    match selected_root_object(doc, META, LEGACY_META) {
        ObjectAt::Missing => true,
        ObjectAt::Object(object) => {
            matches!(string_at(doc, &object, "id"), StringAt::Missing)
                && matches!(string_at(doc, &object, "kind"), StringAt::Missing)
                && !matches!(string_at(doc, &object, "home"), StringAt::Invalid)
                && !matches!(string_at(doc, &object, ROOM), StringAt::Invalid)
                && !matches!(
                    object_at(doc, &object, "refs", ObjType::List),
                    ObjectAt::Invalid
                )
        }
        ObjectAt::Invalid => false,
    }
}

fn validate_identity(
    doc: &AutoCommit,
    want: &RecordCheckout,
    workspace: &str,
) -> Result<(), RecordOpenError> {
    match selected_root_object(doc, META, LEGACY_META) {
        ObjectAt::Object(metadata) => {
            if matches!(
                object_at(doc, &metadata, "refs", ObjType::List),
                ObjectAt::Invalid
            ) {
                return Err(RecordOpenError::IdentityMismatch);
            }
        }
        ObjectAt::Missing if want.id.is_package_primary() => {}
        ObjectAt::Missing | ObjectAt::Invalid => {
            return Err(RecordOpenError::IdentityMismatch);
        }
    }
    if want.id.is_package_primary() {
        return if primary_document(doc)
            && !want.kind.as_str().is_empty()
            && !workspace.is_empty()
            && room_scope(doc).is_none_or(|room| room == workspace)
        {
            Ok(())
        } else {
            Err(RecordOpenError::IdentityMismatch)
        };
    }
    if identity(doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
        || want.kind.as_str().is_empty()
        || home(doc).is_none_or(|home| home.is_empty())
        || room_scope(doc).as_deref() != Some(workspace)
    {
        return Err(RecordOpenError::IdentityMismatch);
    }
    Ok(())
}

fn exact_string(doc: &AutoCommit, object: &ObjId, key: &str, want: &str) -> bool {
    matches!(string_at(doc, object, key), StringAt::String(found) if found == want)
}

fn exact_uint(doc: &AutoCommit, object: &ObjId, key: &str, want: u64) -> bool {
    let Ok(values) = doc.get_all(object, key) else {
        return false;
    };
    matches!(
        values.as_slice(),
        [(Value::Scalar(value), _)] if matches!(value.as_ref(), ScalarValue::Uint(found) if *found == want)
    )
}

fn missing_value(doc: &AutoCommit, object: &ObjId, key: &str) -> bool {
    matches!(doc.get_all(object, key), Ok(values) if values.is_empty())
}

fn validate_qos_notes_receipt(
    doc: &AutoCommit,
    profile: &LocalCompatibilityProfile,
    checkout: &RecordCheckout,
) -> bool {
    let LocalCompatibilityProfile::QosNotesV1 { source_key } = profile;
    let receipt = match object_at(doc, &ROOT, "notes_import_v1", ObjType::Map) {
        ObjectAt::Missing => return true,
        ObjectAt::Object(receipt) => receipt,
        ObjectAt::Invalid => return false,
    };
    let mut keys = doc.keys(&receipt).collect::<Vec<_>>();
    keys.sort();
    if keys
        != [
            "destination_id",
            "format",
            "snapshot_digest",
            "source_key",
            "version",
        ]
    {
        return false;
    }
    let snapshot = match string_at(doc, &receipt, "snapshot_digest") {
        StringAt::String(value) => value,
        StringAt::Missing | StringAt::Invalid => return false,
    };
    if !exact_string(doc, &receipt, "source_key", source_key)
        || !exact_string(doc, &receipt, "destination_id", checkout.id.as_str())
        || !exact_string(doc, &receipt, "format", "workspace-json-markdown-v1")
        || !lower_hex_64(&snapshot)
    {
        return false;
    }
    let Ok(values) = doc.get_all(&receipt, "version") else {
        return false;
    };
    matches!(values.as_slice(), [(Value::Scalar(value), _)] if value.to_u64() == Some(1))
}

fn validate_local_compatibility(
    doc: &AutoCommit,
    evidence: &LocalCanonicalCompatibility,
    package: &str,
    workspace: &str,
) -> Result<(), RecordOpenError> {
    if package != "dev.qos.notes"
        || workspace != evidence.workspace
        || workspace.is_empty()
        || evidence.checkout.kind.as_str() != "notes.notebook"
        || RecordId::parse(evidence.checkout.id.as_str()).is_some()
        || !exact_uint(doc, &ROOT, "qos:schema_version", 1)
        || !missing_value(doc, &ROOT, app_state::SCHEMA_VERSION_KEY)
    {
        return Err(RecordOpenError::IdentityMismatch);
    }
    if !matches!(object_at(doc, &ROOT, META, ObjType::Map), ObjectAt::Missing) {
        return Err(RecordOpenError::IdentityMismatch);
    }
    let ObjectAt::Object(metadata) = object_at(doc, &ROOT, LEGACY_META, ObjType::Map) else {
        return Err(RecordOpenError::IdentityMismatch);
    };
    if !exact_string(doc, &metadata, "id", evidence.checkout.id.as_str())
        || !exact_string(doc, &metadata, "kind", "notes.notebook")
        || !exact_string(doc, &metadata, "home", workspace)
        || !exact_string(doc, &metadata, ROOM, workspace)
        || !matches!(object_at(doc, &metadata, "refs", ObjType::List), ObjectAt::Object(refs) if doc.length(&refs) == 0)
        || !validate_qos_notes_receipt(doc, &evidence.profile, &evidence.checkout)
    {
        return Err(RecordOpenError::IdentityMismatch);
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct RecordAdmission {
    checkout: RecordCheckout,
    workspace: String,
    compatibility: Option<LocalCanonicalCompatibility>,
}

impl RecordAdmission {
    pub(crate) fn matches(&self, doc: &AutoCommit, checkout: &RecordCheckout) -> bool {
        if &self.checkout != checkout {
            return false;
        }
        if let Some(evidence) = &self.compatibility {
            return validate_local_compatibility(doc, evidence, "dev.qos.notes", &self.workspace)
                .is_ok();
        }
        identity(doc).as_ref() == Some(checkout)
    }
}

/// The unique construction owner of a not-yet-live record. Drop cancels only this generation.
pub struct RecordReservation {
    store: RecordStore,
    checkout: RecordCheckout,
    generation: u64,
}

impl RecordReservation {
    pub fn checkout(&self) -> &RecordCheckout {
        &self.checkout
    }

    /// Privately initialize once, then create the one history/writer and publish Ready.
    ///
    /// # Errors
    /// Retains the reservation on initializer, validation or worker-startup failure.
    pub fn initialize(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        if self.store.0.borrow().portable.is_some() {
            return self.initialize_portable(initialize);
        }
        self.initialize_with_spawn(initialize, |owner| {
            app_state::Autosave::try_spawn_canonical(owner, app_state::DEFAULT_DEBOUNCE)
        })
    }

    fn initialize_with_spawn(
        &self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        spawn: impl FnOnce(
            &std::sync::Arc<app_state::DocStore>,
        ) -> Result<app_state::Autosave, app_state::StoreError>,
    ) -> Result<(), RecordOpenError> {
        let (owner, workspace) = {
            let set = self.store.0.borrow();
            let pending = set
                .pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(RecordOpenError::Busy)?;
            (
                std::sync::Arc::clone(
                    pending
                        .store
                        .as_ref()
                        .ok_or(RecordOpenError::IdentityMismatch)?,
                ),
                set.home.clone(),
            )
        };
        let mut doc = owner.new_canonical_document()?;
        if !init_scoped(
            &mut doc,
            &self.checkout.id,
            &self.checkout.kind,
            &workspace,
            Some(&workspace),
        ) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        initialize(&mut doc).map_err(RecordOpenError::Initialization)?;
        owner.validate_canonical_schema(&doc)?;
        validate_identity(&doc, &self.checkout, &workspace)?;
        doc.commit();
        let autosave = spawn(&owner)?;
        let history = crate::state::UndoLog::new(&mut doc);
        let mut set = self.store.0.borrow_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_none_or(|p| p.generation != self.generation)
        {
            return Err(RecordOpenError::Busy);
        }
        set.pending.remove(&self.checkout.id);
        set.open.insert(
            self.checkout.id.clone(),
            Parked {
                doc: Some(doc),
                kind: self.checkout.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: crate::bound_dispatch::RecordActionState::default(),
            },
        );
        if let Some(slot) = set.open.get_mut(&self.checkout.id)
            && let (Some(doc), Some(saver)) = (&mut slot.doc, &mut slot.autosave)
        {
            saver.request(doc);
        }
        Ok(())
    }
}

impl Drop for RecordReservation {
    fn drop(&mut self) {
        let mut set = self.store.0.borrow_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_some_and(|p| p.generation == self.generation)
        {
            set.pending.remove(&self.checkout.id);
        }
    }
}

impl RecordStore {
    /// Checked custody-to-wire mapping. Primary stays `record=None` on every host.
    pub fn route(
        &self,
        checkout: &RecordCheckout,
    ) -> Result<crate::collaboration::RouteSpec, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        if set.local_compatibility.contains_key(&checkout.id) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let doc = set
            .peek(&checkout.id)
            .ok_or(RecordOpenError::MissingRecord)?;
        validate_identity(doc, checkout, &set.home)?;
        let room =
            uuid::Uuid::parse_str(&set.home).map_err(|_| RecordOpenError::IdentityMismatch)?;
        Ok(if checkout.id.is_package_primary() {
            crate::collaboration::RouteSpec::app(&set.package_id, room)
        } else {
            crate::collaboration::RouteSpec::record(&set.package_id, room, checkout.id.as_str())
        })
    }

    /// Schedule this already committed History result through its existing writer.
    /// Do not sweep or close unrelated owners' unfinished edits.
    pub(crate) fn schedule_committed_history(&self, id: &RecordId) {
        let Ok(mut set) = self.0.try_borrow_mut() else {
            return;
        };
        if set.portable.is_some() {
            if set
                .open
                .get(id)
                .and_then(|slot| slot.doc.as_ref())
                .is_some_and(|doc| doc.pending_ops() == 0)
            {
                let _ = set.flush_portable(Some(id));
            }
            return;
        }
        let Some(slot) = set.open.get_mut(id) else {
            return;
        };
        if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut())
            && doc.pending_ops() == 0
        {
            saver.request(doc);
        }
    }

    /// Captured storage identity for History; never samples ambient launch scope.
    pub(crate) fn history_namespace(
        &self,
        id: &RecordId,
    ) -> Result<(Option<String>, String, String), RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        let storage = set.canonical_root.clone().or_else(|| {
            parked
                .autosave
                .as_ref()
                .map(|save| save.document_path().to_path_buf())
        });
        let storage = set
            .portable
            .as_ref()
            .map(|cache| format!("portable:{:?}", cache.namespace()))
            .or_else(|| storage.map(|path| path.to_string_lossy().into_owned()));
        Ok((storage, set.package_id.clone(), set.home.clone()))
    }
    /// Cheap content witness for retained projections over open records.
    ///
    /// Base heads cover committed changes and `pending_ops` covers the live
    /// typing transaction before its next commit boundary. Record identity is
    /// included so opening or closing a document also invalidates a snapshot.
    pub fn content_revision(&self) -> String {
        let set = self.0.borrow();
        let mut records = set.open.iter().collect::<Vec<_>>();
        records.sort_unstable_by_key(|(id, _)| *id);
        let mut revision = String::new();
        for (id, parked) in records {
            use std::fmt::Write as _;
            let Some(doc) = parked.doc.as_ref() else {
                continue;
            };
            let _ = write!(
                revision,
                "{}:{:?}:{}\u{1e}",
                id.as_str(),
                doc.base_heads(),
                doc.pending_ops()
            );
        }
        revision
    }

    /// Explicit immutable identity only; legacy and memory stores have no canonical namespace.
    pub(crate) fn canonical_namespace(&self) -> Option<(ProfileNamespace, String, String)> {
        let set = self.0.borrow();
        Some((
            set.portable
                .as_ref()
                .map(|cache| cache.namespace().clone())
                .or_else(|| set.canonical_root.clone().map(ProfileNamespace::Native))?,
            set.package_id.clone(),
            set.home.clone(),
        ))
    }

    /// Open Existing only; a missing document is never implicitly initialized.
    ///
    /// # Errors
    /// Reports ownership contention, absence, identity/schema refusal and storage failures.
    pub fn try_open_record(
        &self,
        want: &RecordCheckout,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        self.0.borrow_mut().try_open_record(want)
    }

    /// Open one exact, already persisted local compatibility record. This
    /// admission does not make its historical ID valid for generic opens,
    /// creation, room discovery or collaboration.
    pub fn try_open_local_compatibility(
        &self,
        evidence: &LocalCanonicalCompatibility,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        self.0.borrow_mut().try_open_local_compatibility(evidence)
    }

    /// Reserve a New identity with its exclusive gate and no document/history/writer yet.
    ///
    /// # Errors
    /// Refuses collisions, malformed existing data and unavailable storage.
    pub fn reserve_record(
        &self,
        want: &RecordCheckout,
    ) -> Result<RecordReservation, RecordOpenError> {
        let mut set = self.0.borrow_mut();
        if !want.is_valid_local_storage_request() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        if set.portable.is_some() {
            return self.reserve_portable(&mut set, want);
        }
        let directory = set.canonical_directory(want)?;
        if set.pending.contains_key(&want.id) || set.open.contains_key(&want.id) {
            return Err(RecordOpenError::Busy);
        }
        let owner = app_state::DocStore::open_canonical_at(&set.package_id, &directory)?;
        if owner.load_canonical()?.is_some() {
            return Err(RecordOpenError::Busy);
        }
        let generation = set
            .next_reservation
            .checked_add(1)
            .ok_or(RecordOpenError::Busy)?;
        set.next_reservation = generation;
        set.pending.insert(
            want.id.clone(),
            Reserved {
                generation,
                store: Some(std::sync::Arc::new(owner)),
                actor: None,
            },
        );
        Ok(RecordReservation {
            store: self.clone(),
            checkout: want.clone(),
            generation,
        })
    }

    pub fn is_ready(&self, id: &RecordId) -> bool {
        self.0.borrow().peek(id).is_some()
    }

    /// Inspect durability without closing a typing transaction or scheduling work.
    ///
    /// # Errors
    /// Reports missing/reserved records.
    pub fn save_status(&self, id: &RecordId) -> Result<RecordSaveStatus, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        if set.pending.contains_key(id) {
            return Err(RecordOpenError::Busy);
        }
        let slot = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
        if let Some(cache) = &set.portable {
            return Ok(portable::save_status(
                cache,
                &set.package_id,
                &set.home,
                id,
                doc,
            ));
        }
        if let Some(saver) = &slot.autosave {
            let snapshot = saver.durability_snapshot();
            if let Some(error) = snapshot.error {
                return Ok(RecordSaveStatus::Failed(error));
            }
            return Ok(
                if doc.pending_ops() == 0
                    && snapshot.durable_heads.as_ref() == Some(&doc.base_heads())
                {
                    RecordSaveStatus::Saved
                } else {
                    RecordSaveStatus::Saving
                },
            );
        }
        Ok(if set.persistent {
            RecordSaveStatus::Failed("record persistence unavailable".to_owned())
        } else {
            RecordSaveStatus::MemoryOnly
        })
    }

    pub fn durability_error(&self, id: &RecordId) -> Option<String> {
        if self.0.borrow().portable.is_some() {
            return match self.save_status(id) {
                Ok(RecordSaveStatus::Failed(error)) => Some(error),
                _ => None,
            };
        }
        self.0
            .borrow()
            .open
            .get(id)
            .and_then(|p| p.autosave.as_ref())
            .and_then(app_state::Autosave::last_error)
    }

    /// Close/retry one actual owner without settling another record's edit.
    pub(crate) fn try_flush_record(&self, id: &RecordId) -> Result<(), String> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| "record store is busy".to_string())?;
        if set.portable.is_some() {
            return set.flush_portable(Some(id));
        }
        let canonical = set.canonical_root.is_some();
        let slot = set
            .open
            .get_mut(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let doc = slot
            .doc
            .as_mut()
            .ok_or_else(|| format!("{id}: record is busy"))?;
        let Some(saver) = &mut slot.autosave else {
            return Ok(());
        };
        if canonical && saver.last_error().is_some() {
            saver
                .try_restart_canonical()
                .map_err(|error| format!("{id}: {error}"))?;
            slot.action_state.commit_for_save(doc);
            saver.request_checkpoint(doc);
        } else {
            slot.action_state.commit_for_save(doc);
            saver.request(doc);
        }
        saver.try_flush().map_err(|error| format!("{id}: {error}"))
    }

    /// Request current changes once and await actual acknowledgements for every record.
    ///
    /// # Errors
    /// Returns record-specific storage failures; a subsequent explicit retry sends a full checkpoint.
    pub fn try_flush(&self) -> Result<(), String> {
        let mut set = self.0.borrow_mut();
        if set.portable.is_some() {
            return set.flush_portable(None);
        }
        set.try_flush_documents(None)
    }

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

    /// Create exactly the new resource named by its owning action. Reopening
    /// existing tabs must use existing-only admission instead.
    ///
    /// # Errors
    /// Refuses existing identities, unavailable persistence, and invalid scope.
    pub fn create_record(&self, want: &RecordCheckout) -> Result<(), RecordOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let canonical = {
            let set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
            set.canonical_root.is_some() || set.portable.is_some()
        };
        if canonical {
            return self
                .reserve_record(want)
                .and_then(|mut reservation| reservation.initialize(|_| Ok(())))
                .map_err(|error| match error {
                    RecordOpenError::Busy => RecordOpenError::AlreadyExists,
                    other => other,
                });
        }
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if set.open.contains_key(&want.id) {
            return Err(RecordOpenError::AlreadyExists);
        }
        if want.kind.as_str().is_empty() || (set.persistent && set.home.is_empty()) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let owner = if set.persistent {
            let mut owner = app_state::DocStore::open_room_record(
                &set.package_id,
                &set.home,
                &want.id.storage_key(),
            )?;
            // An empty/corrupt file is existing evidence too, never permission
            // to replace it with a freshly initialized document.
            if owner.load_complete()?.is_some()
                || owner.document_path().try_exists().map_err(|source| {
                    app_state::StoreError::Io {
                        context: "checking new record identity".into(),
                        source,
                    }
                })?
                || app_state::DocStore::legacy_record_document_path(
                    &set.package_id,
                    &want.id.storage_key(),
                )?
                .try_exists()
                .map_err(|source| app_state::StoreError::Io {
                    context: "checking legacy record identity".into(),
                    source,
                })?
            {
                return Err(RecordOpenError::AlreadyExists);
            }
            Some(owner)
        } else {
            None
        };
        let mut owner = owner;
        let mut doc = owner
            .as_mut()
            .map_or_else(AutoCommit::new, app_state::DocStore::new_document);
        let scope = (!set.home.is_empty()).then_some(set.home.as_str());
        if !init_scoped(&mut doc, &want.id, &want.kind, &set.home, scope) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        for (target, relation) in &want.references {
            add_reference(
                &mut doc,
                &RecordRef::new(target.id.clone(), relation.clone()),
            );
        }
        doc.commit();
        let mut autosave = owner
            .map(|owner| crate::persist::Autosave::spawn(owner, crate::persist::DEFAULT_DEBOUNCE));
        if let Some(error) = autosave
            .as_ref()
            .and_then(|saver| saver.durability_snapshot().error)
        {
            return Err(RecordOpenError::Initialization(error));
        }
        let history = crate::state::UndoLog::new(&mut doc);
        if let Some(saver) = &mut autosave {
            saver.request(&mut doc);
        }
        set.open.insert(
            want.id.clone(),
            Parked {
                doc: Some(doc),
                kind: want.kind.clone(),
                autosave,
                history,
                action_state: crate::bound_dispatch::RecordActionState::default(),
            },
        );
        Ok(())
    }

    /// Resolve legacy tile intent without panicking on an overlapping record borrow.
    /// Canonical stores retain their Existing-only identity/storage errors;
    /// legacy stores retain the existing open/create behavior.
    pub(crate) fn open_record_checked(&self, want: &RecordCheckout) -> Result<(), RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if set.canonical_root.is_some() || set.portable.is_some() {
            return set.try_open_record_with_recovery(want, false).map(|_| ());
        }
        set.open_record(want, None);
        set.document_mut(&want.id)
            .map(|_| ())
            .ok_or(RecordOpenError::MissingRecord)
    }

    /// Existing-only admission for presentation owners. Loads a saved graph
    /// without seeding missing content. Package-global records retain the
    /// existing explicit one-room migration claim and their original history.
    pub(crate) fn open_existing_record_checked(
        &self,
        want: &RecordCheckout,
    ) -> Result<(), RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if let Some(evidence) = set.local_compatibility.get(&want.id).cloned() {
            if &evidence.checkout != want {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let slot = set
                .open
                .get(&want.id)
                .ok_or(RecordOpenError::MissingRecord)?;
            let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
            return validate_local_compatibility(doc, &evidence, &set.package_id, &set.home);
        }
        if set.canonical_root.is_some() || set.portable.is_some() {
            return set.try_open_record_with_recovery(want, false).map(|_| ());
        }
        if !set.open.contains_key(&want.id) {
            if !set.persistent {
                return Err(RecordOpenError::MissingRecord);
            }
            if set.home.is_empty() {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let mut owner = app_state::DocStore::open_room_record(
                &set.package_id,
                &set.home,
                &want.id.storage_key(),
            )?;
            let (mut doc, adopted) = if let Some(doc) = owner.load_complete()? {
                (doc, false)
            } else {
                if owner.document_path().try_exists().map_err(|source| {
                    app_state::StoreError::Io {
                        context: "checking existing record".into(),
                        source,
                    }
                })? {
                    return Err(app_state::StoreError::EmptyExistingDocument {
                        path: owner.document_path().to_path_buf(),
                    }
                    .into());
                }
                let key = want.id.storage_key();
                let path = app_state::DocStore::legacy_record_document_path(&set.package_id, &key)?;
                if !path
                    .try_exists()
                    .map_err(|source| app_state::StoreError::Io {
                        context: "checking legacy migration source".into(),
                        source,
                    })?
                {
                    return Err(RecordOpenError::MissingRecord);
                }
                let mut legacy = app_state::DocStore::open_record(&set.package_id, &key)?;
                let mut doc = legacy.load_complete()?.ok_or_else(|| {
                    app_state::StoreError::EmptyExistingDocument { path: path.clone() }
                })?;
                // Validate before claiming custody or adding only the missing
                // room metadata. The old source and all its changes stay intact.
                if identity(&doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
                    || home(&doc).as_deref() != Some(set.home.as_str())
                    || room_scope(&doc).is_some_and(|scope| scope != set.home)
                    || !init_scoped(&mut doc, &want.id, &want.kind, &set.home, Some(&set.home))
                    || !app_state::DocStore::claim_legacy_record_migration(
                        &set.package_id,
                        &set.home,
                        &key,
                    )?
                {
                    return Err(RecordOpenError::IdentityMismatch);
                }
                (doc, true)
            };
            validate_identity(&doc, want, &set.home)?;
            if home(&doc).as_deref() != Some(set.home.as_str()) {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let mut autosave =
                crate::persist::Autosave::spawn(owner, crate::persist::DEFAULT_DEBOUNCE);
            if let Some(error) = autosave.durability_snapshot().error {
                return Err(RecordOpenError::Initialization(error));
            }
            if !adopted {
                autosave.seed(&mut doc);
            }
            let history = crate::state::UndoLog::new(&mut doc);
            if adopted {
                autosave.request(&mut doc);
            }
            set.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave: Some(autosave),
                    history,
                    action_state: crate::bound_dispatch::RecordActionState::default(),
                },
            );
        }
        let slot = set
            .open
            .get(&want.id)
            .ok_or(RecordOpenError::MissingRecord)?;
        let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
        // Legacy in-memory owners legitimately have no workspace. Match that
        // captured namespace exactly; do not manufacture a canonical scope.
        let expected_scope = (!set.home.is_empty()).then_some(set.home.as_str());
        if identity(doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
            || want.kind.as_str().is_empty()
            || home(doc).as_deref() != Some(set.home.as_str())
            || room_scope(doc).as_deref() != expected_scope
        {
            return Err(RecordOpenError::IdentityMismatch);
        }
        Ok(())
    }

    /// Exact runtime owner identity; namespace equality is insufficient for reentry.
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn is_local_compatibility(&self, checkout: &RecordCheckout) -> bool {
        let Ok(set) = self.0.try_borrow() else {
            return false;
        };
        set.local_compatibility
            .get(&checkout.id)
            .is_some_and(|evidence| evidence.checkout == *checkout)
    }

    pub(crate) fn verified_checkout(
        &self,
        id: &RecordId,
    ) -> Result<RecordCheckout, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let slot = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
        if let Some(evidence) = set.local_compatibility.get(id) {
            validate_local_compatibility(doc, evidence, &set.package_id, &set.home)?;
            return Ok(evidence.checkout.clone());
        }
        let checkout = identity(doc).ok_or(RecordOpenError::IdentityMismatch)?;
        if checkout.id != *id || checkout.kind != slot.kind {
            return Err(RecordOpenError::IdentityMismatch);
        }
        Ok(checkout)
    }

    pub(crate) fn admission(
        &self,
        checkout: &RecordCheckout,
    ) -> Result<RecordAdmission, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let slot = set
            .open
            .get(&checkout.id)
            .ok_or(RecordOpenError::MissingRecord)?;
        let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
        let compatibility = set.local_compatibility.get(&checkout.id).cloned();
        if let Some(evidence) = &compatibility {
            if &evidence.checkout != checkout {
                return Err(RecordOpenError::IdentityMismatch);
            }
            validate_local_compatibility(doc, evidence, &set.package_id, &set.home)?;
        } else {
            if identity(doc).as_ref() != Some(checkout) {
                return Err(RecordOpenError::IdentityMismatch);
            }
        }
        Ok(RecordAdmission {
            checkout: checkout.clone(),
            workspace: set.home.clone(),
            compatibility,
        })
    }

    pub(crate) fn with_checked<R>(
        &self,
        id: &RecordId,
        f: impl FnOnce(&mut AutoCommit) -> R,
    ) -> Result<R, RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        let document = set.document_mut(id).ok_or(RecordOpenError::MissingRecord)?;
        Ok(f(document))
    }

    pub(crate) fn read_checked<R>(
        &self,
        id: &RecordId,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let document = set
            .open
            .get(id)
            .and_then(|parked| parked.doc.as_ref())
            .ok_or(RecordOpenError::MissingRecord)?;
        Ok(read(document))
    }

    /// One exclusive record admission for the document and owned action frame.
    pub(crate) fn with_action_state<R>(
        &self,
        id: &RecordId,
        callback: impl FnOnce(
            &mut AutoCommit,
            &mut crate::state::UndoLog,
            &mut crate::bound_dispatch::RecordActionState,
        ) -> R,
    ) -> Result<R, RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        let parked = set.open.get_mut(id).ok_or(RecordOpenError::MissingRecord)?;
        let doc = parked.doc.as_mut().ok_or(RecordOpenError::MissingRecord)?;
        Ok(callback(doc, &mut parked.history, &mut parked.action_state))
    }

    /// Generation of the record-owned action frame, for retained callers that
    /// need to invalidate a previously resolved operation.
    ///
    /// # Errors
    /// Returns `Busy` if the record set is borrowed, or `MissingRecord` if the record is not open.
    pub fn action_generation(&self, id: &RecordId) -> Result<Option<u64>, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        Ok(parked.action_state.generation)
    }

    /// Capture the exact operational identity before admitting remote bytes.
    pub(crate) fn replica_identity_stamp(
        &self,
        checkout: &RecordCheckout,
    ) -> Result<ReplicaIdentityStamp, String> {
        let (_, _, workspace) = self
            .history_namespace(&checkout.id)
            .map_err(|error| error.to_string())?;
        self.read_checked(&checkout.id, |doc| {
            ReplicaIdentityStamp::capture(doc, checkout, &workspace)
        })
        .map_err(|error| error.to_string())?
    }

    /// Merge broker-admitted bytes atomically into the canonical owner. Validate
    /// on a private copy before ending local typing or changing the undo baseline.
    #[cfg(test)]
    pub(crate) fn merge_remote_document(
        &self,
        checkout: &RecordCheckout,
        mutation: collab_types::DocumentMutation,
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        let identity = self.replica_identity_stamp(checkout)?;
        self.with_action_state(&checkout.id, |doc, history, actions| {
            let mut local = doc.clone();
            actions.commit_for_save(&mut local);
            let before = local.get_heads();
            let mut candidate = local.clone();
            let observed = match mutation {
                collab_types::DocumentMutation::Snapshot(bytes) => {
                    let mut incoming =
                        AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
                    identity.validate(&incoming)?;
                    let heads = incoming.get_heads();
                    candidate
                        .merge(&mut incoming)
                        .map_err(|error| error.to_string())?;
                    heads
                }
                collab_types::DocumentMutation::Incremental(bytes) => {
                    // load_incremental deliberately accepts a valid prefix of
                    // corrupt input. Validate the entire payload first. Missing
                    // dependencies are valid for an incremental message and are
                    // checked only after Automerge has parsed every chunk.
                    match AutoCommit::load_with_options(
                        &bytes,
                        automerge::LoadOptions::new()
                            .on_partial_load(automerge::OnPartialLoad::Error),
                    ) {
                        Ok(_) | Err(automerge::AutomergeError::MissingDeps) => {}
                        Err(error) => return Err(error.to_string()),
                    }
                    candidate
                        .load_incremental(&bytes)
                        .map_err(|error| error.to_string())?;
                    candidate
                        .get_changes(&before)
                        .iter()
                        .map(automerge::Change::hash)
                        .collect()
                }
            };
            identity.validate(&candidate)?;
            let changed = candidate.get_heads() != before;
            if changed {
                // Finish the local span against LOCAL heads, never the merged
                // heads: undoing it must leave the remote author's edits intact.
                actions.finish_external(&mut local, history);
                history.rebase(&mut candidate);
                actions.generation = actions.generation.and_then(|value| value.checked_add(1));
            }
            // Keep valid changes with dependencies still in flight, too.
            *doc = candidate;
            Ok((changed, observed))
        })
        .map_err(|error| error.to_string())?
    }

    /// Do something with a record's document. `None` if the record is not open here.
    pub fn with<R>(&self, id: &RecordId, f: impl FnOnce(&mut AutoCommit) -> R) -> Option<R> {
        let mut set = self.0.borrow_mut();
        set.document_mut(id).map(f)
    }

    /// Read a record's document.
    /// Merge an already admitted remote mutation into the one record owner.
    /// Preserves unfinished local work and rebases shared undo only after merge.
    pub fn can_merge_admitted(&self, checkout: &RecordCheckout) -> Result<bool, String> {
        crate::bound_dispatch::can_merge_remote_owned_document(self, checkout)
            .map_err(|error| error.to_string())
    }
    pub fn merge_admitted(
        &self,
        checkout: &RecordCheckout,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<Option<bool>, String> {
        let changed = crate::bound_dispatch::merge_remote_owned_document(self, checkout, mutation)
            .map_err(|e| e.to_string())?;
        if changed == Some(true) {
            let _ = self.try_flush();
        }
        Ok(changed)
    }
    pub fn read<R>(&self, id: &RecordId, f: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        self.read_checked(id, f).ok()
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

    /// Execute one externally addressed document action through the record's
    /// same owned frame as its mounted application. The frame preserves the
    /// action generation, author, application instance, semantic context, and
    /// one undo boundary.
    ///
    /// # Errors
    /// Refuses a record that is absent, busy, or no longer owned by this store.
    pub fn with_owned_action<R>(
        &self,
        id: &RecordId,
        instance_id: crate::application::InstanceId,
        app_id: &str,
        author: crate::permissions::UserId,
        context: tilo_actions::ActionContext,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, RecordOpenError> {
        let checkout = self
            .open_records()
            .into_iter()
            .find(|checkout| checkout.id == *id)
            .ok_or(RecordOpenError::MissingRecord)?;
        crate::bound_dispatch::with_owned_record_action(
            self,
            &checkout,
            instance_id,
            app_id,
            author,
            context,
            callback,
        )
        .map_err(|error| match error {
            crate::bound_dispatch::BoundDispatchError::Record(error) => error,
            crate::bound_dispatch::BoundDispatchError::MissingNode
            | crate::bound_dispatch::BoundDispatchError::ClosedLifetime
            | crate::bound_dispatch::BoundDispatchError::CrossRecordReentry => {
                RecordOpenError::Busy
            }
        })
    }

    /// Inspect committed history without turning a pending burst into a boundary.
    pub fn read_history<R>(
        &self,
        id: &RecordId,
        read: impl FnOnce(&AutoCommit, &crate::state::UndoLog) -> R,
    ) -> Option<R> {
        let set = self.0.try_borrow().ok()?;
        let parked = set.open.get(id)?;
        Some(read(parked.doc.as_ref()?, &parked.history))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordMetadataFamily {
    Tilo,
    Qos,
}

fn record_metadata_family(doc: &AutoCommit) -> Result<Option<RecordMetadataFamily>, String> {
    match (
        object_at(doc, &ROOT, META, ObjType::Map),
        object_at(doc, &ROOT, LEGACY_META, ObjType::Map),
    ) {
        (ObjectAt::Missing, ObjectAt::Missing) => Ok(None),
        (ObjectAt::Object(_), ObjectAt::Missing) => Ok(Some(RecordMetadataFamily::Tilo)),
        (ObjectAt::Missing, ObjectAt::Object(_)) => Ok(Some(RecordMetadataFamily::Qos)),
        _ => Err("conflicting record metadata namespace".into()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReplicaIdentityStamp {
    checkout: RecordCheckout,
    workspace: String,
    family: Option<RecordMetadataFamily>,
    home: Option<String>,
    room: Option<String>,
}

impl ReplicaIdentityStamp {
    fn capture(
        doc: &AutoCommit,
        checkout: &RecordCheckout,
        workspace: &str,
    ) -> Result<Self, String> {
        validate_identity(doc, checkout, workspace).map_err(|error| error.to_string())?;
        let family = record_metadata_family(doc)?;
        if !checkout.id.is_package_primary() && family.is_none() {
            return Err("record metadata is missing".into());
        }
        Ok(Self {
            checkout: checkout.clone(),
            workspace: workspace.to_string(),
            family,
            home: home(doc),
            room: room_scope(doc),
        })
    }

    pub(crate) fn validate(&self, doc: &AutoCommit) -> Result<(), String> {
        validate_identity(doc, &self.checkout, &self.workspace)
            .map_err(|error| error.to_string())?;
        if record_metadata_family(doc)? != self.family
            || home(doc) != self.home
            || room_scope(doc) != self.room
        {
            return Err("conflicting record identity".into());
        }
        Ok(())
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

    fn qos_notes_v1_evidence() -> LocalCanonicalCompatibility {
        let source_key = "31".repeat(32);
        LocalCanonicalCompatibility::qos_notes_v1(
            "compat-room",
            &source_key,
            &format!("derived:notes-legacy-v1@{source_key}"),
        )
        .unwrap()
    }

    fn qos_notes_v1_document(evidence: &LocalCanonicalCompatibility, receipt: bool) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "qos:schema_version", 1_u64).unwrap();
        let meta = doc.put_object(ROOT, LEGACY_META, ObjType::Map).unwrap();
        for (key, value) in [
            ("id", evidence.checkout.id.as_str()),
            ("kind", "notes.notebook"),
            ("home", evidence.workspace.as_str()),
            (ROOM, evidence.workspace.as_str()),
        ] {
            doc.put(&meta, key, value).unwrap();
        }
        doc.put_object(&meta, "refs", ObjType::List).unwrap();
        if receipt {
            let LocalCompatibilityProfile::QosNotesV1 { source_key } = &evidence.profile;
            let entry = doc
                .put_object(ROOT, "notes_import_v1", ObjType::Map)
                .unwrap();
            let snapshot_digest = "42".repeat(32);
            for (key, value) in [
                ("source_key", source_key.as_str()),
                ("snapshot_digest", snapshot_digest.as_str()),
                ("destination_id", evidence.checkout.id.as_str()),
                ("format", "workspace-json-markdown-v1"),
            ] {
                doc.put(&entry, key, value).unwrap();
            }
            doc.put(&entry, "version", 1_u64).unwrap();
        }
        doc
    }

    fn write_qos_notes_v1(
        state_root: &Path,
        evidence: &LocalCanonicalCompatibility,
        receipt: bool,
    ) -> PathBuf {
        std::fs::create_dir_all(state_root).unwrap();
        let directory = app_state::DocStore::room_record_dir_at(
            state_root,
            "dev.qos.notes",
            &evidence.workspace,
            &evidence.checkout.id.storage_key(),
        )
        .unwrap();
        drop(app_state::DocStore::open_canonical_at("dev.qos.notes", &directory).unwrap());
        let mut doc = qos_notes_v1_document(evidence, receipt);
        let path = directory.join(app_state::DOCUMENT_FILE);
        std::fs::write(&path, doc.save()).unwrap();
        path
    }

    #[test]
    fn qos_notes_v1_is_existing_only_local_and_keeps_generic_boundaries_closed() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let evidence = qos_notes_v1_evidence();
        let document = write_qos_notes_v1(&state, &evidence, true);
        let original = std::fs::read(&document).unwrap();
        let mut owners = RecordOwners::new(state);
        let store = owners.open("dev.qos.notes", "compat-room").unwrap();

        assert!(RecordId::parse(evidence.checkout.id.as_str()).is_none());
        assert!(matches!(
            store.try_open_record(&evidence.checkout),
            Err(RecordOpenError::IdentityMismatch)
        ));
        assert_eq!(
            store.try_open_local_compatibility(&evidence).unwrap(),
            RecordOpenOutcome::Loaded
        );
        assert_eq!(
            store.try_open_local_compatibility(&evidence).unwrap(),
            RecordOpenOutcome::AlreadyOpen
        );
        assert_eq!(
            store.verified_checkout(&evidence.checkout.id).unwrap(),
            evidence.checkout
        );
        assert!(store.route(&evidence.checkout).is_err());
        assert!(store.can_merge_admitted(&evidence.checkout).is_err());
        assert!(
            store
                .merge_admitted(
                    &evidence.checkout,
                    &collab_types::DocumentMutation::Snapshot(original.clone())
                )
                .is_err()
        );
        store.try_flush().unwrap();
        assert_eq!(std::fs::read(document).unwrap(), original);

        let mut wrong = RecordOwners::new(temp.path().join("wrong"));
        let wrong = wrong.open("dev.qos.notes", "wrong-room").unwrap();
        assert!(matches!(
            wrong.try_open_local_compatibility(&evidence),
            Err(RecordOpenError::IdentityMismatch)
        ));
    }

    #[test]
    fn qos_notes_v1_evidence_and_document_tampering_fail_closed() {
        let key = "ab".repeat(32);
        let record = format!("derived:notes-legacy-v1@{key}");
        assert!(LocalCanonicalCompatibility::qos_notes_v1("", &key, &record).is_none());
        let uppercase_key = key.to_ascii_uppercase();
        let uppercase_record = format!("derived:notes-legacy-v1@{uppercase_key}");
        assert!(
            LocalCanonicalCompatibility::qos_notes_v1(
                "compat-room",
                &uppercase_key,
                &uppercase_record
            )
            .is_none()
        );
        assert!(
            LocalCanonicalCompatibility::qos_notes_v1(
                "compat-room",
                &key,
                &format!("derived:other@{key}")
            )
            .is_none()
        );

        let evidence = qos_notes_v1_evidence();
        let fresh = qos_notes_v1_document(&evidence, false);
        assert!(
            validate_local_compatibility(&fresh, &evidence, "dev.qos.notes", "compat-room").is_ok()
        );
        let mut imported = qos_notes_v1_document(&evidence, true);
        assert!(
            validate_local_compatibility(&imported, &evidence, "dev.qos.notes", "compat-room")
                .is_ok()
        );
        let (_, receipt) = imported.get(ROOT, "notes_import_v1").unwrap().unwrap();
        imported
            .put(&receipt, "snapshot_digest", "AB".repeat(32))
            .unwrap();
        assert!(
            validate_local_compatibility(&imported, &evidence, "dev.qos.notes", "compat-room")
                .is_err()
        );

        let mut missing_schema = qos_notes_v1_document(&evidence, false);
        missing_schema.delete(ROOT, "qos:schema_version").unwrap();
        assert!(
            validate_local_compatibility(
                &missing_schema,
                &evidence,
                "dev.qos.notes",
                "compat-room"
            )
            .is_err()
        );
        let mut current_schema = qos_notes_v1_document(&evidence, false);
        current_schema.delete(ROOT, "qos:schema_version").unwrap();
        current_schema
            .put(ROOT, app_state::SCHEMA_VERSION_KEY, 1_u64)
            .unwrap();
        assert!(
            validate_local_compatibility(
                &current_schema,
                &evidence,
                "dev.qos.notes",
                "compat-room"
            )
            .is_err()
        );
        let mut dual_schema = qos_notes_v1_document(&evidence, false);
        dual_schema
            .put(ROOT, app_state::SCHEMA_VERSION_KEY, 1_u64)
            .unwrap();
        assert!(
            validate_local_compatibility(&dual_schema, &evidence, "dev.qos.notes", "compat-room")
                .is_err()
        );

        for field in ["id", "kind", "home", ROOM] {
            let mut tampered = qos_notes_v1_document(&evidence, false);
            let (_, meta) = tampered.get(ROOT, LEGACY_META).unwrap().unwrap();
            tampered.put(&meta, field, "foreign").unwrap();
            assert!(
                validate_local_compatibility(&tampered, &evidence, "dev.qos.notes", "compat-room")
                    .is_err()
            );
        }
    }

    #[test]
    fn generic_storage_rejects_exposed_historical_id_before_path_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let evidence = qos_notes_v1_evidence();
        let directory = app_state::DocStore::room_record_dir_at(
            &state,
            "dev.qos.notes",
            "compat-room",
            &evidence.checkout.id.storage_key(),
        )
        .unwrap();
        let mut owners = RecordOwners::new(state);
        let store = owners.open("dev.qos.notes", "compat-room").unwrap();
        assert!(matches!(
            store.try_open_record(&evidence.checkout),
            Err(RecordOpenError::IdentityMismatch)
        ));
        assert!(!directory.exists());
        assert!(matches!(
            store.reserve_record(&evidence.checkout),
            Err(RecordOpenError::IdentityMismatch)
        ));
        assert!(!directory.exists());
        let memory = RecordStore::in_memory();
        assert!(matches!(
            memory.create_record(&evidence.checkout),
            Err(RecordOpenError::IdentityMismatch)
        ));
        assert!(memory.open_records().is_empty());
    }

    #[test]
    fn strict_generic_shape_guard_preserves_valid_package_primary() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let mut owners = RecordOwners::new(state.clone());
        let store = owners
            .open("dev.tilo.primary-test", "primary-room")
            .unwrap();
        let primary = RecordCheckout::package_primary(RecordKind::new("primary"));
        store
            .reserve_record(&primary)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_flush().unwrap();
        assert!(store.try_open_record(&primary).is_ok());
        let memory = RecordStore::in_memory();
        memory.create_record(&primary).unwrap();

        let malformed = RecordCheckout::package_primary(RecordKind::new("1invalid"));
        assert!(matches!(
            store.reserve_record(&malformed),
            Err(RecordOpenError::IdentityMismatch)
        ));
        let malformed = RecordCheckout::package_primary(RecordKind::new("primary")).referencing(
            RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("target")),
            "bad relation",
        );
        assert!(matches!(
            store.reserve_record(&malformed),
            Err(RecordOpenError::IdentityMismatch)
        ));
    }

    #[test]
    fn save_status_older_ack_keeps_newer_pending_edit_saving() {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(temp.path().join("state"));
        let store = owners.open("dev.tilo.test", "status-room").unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut reservation = store.reserve_record(&checkout).unwrap();
        reservation
            .initialize(|doc| {
                doc.put(ROOT, "value", 0_u64)
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .unwrap();
        store.try_flush().unwrap();
        store
            .with(&checkout.id, |doc| doc.put(ROOT, "value", 1_u64).unwrap())
            .unwrap();
        store.tick();
        let older = store.read(&checkout.id, AutoCommit::base_heads).unwrap();
        store
            .with(&checkout.id, |doc| doc.put(ROOT, "value", 2_u64).unwrap())
            .unwrap();
        let pending = store
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(pending.1 > 0);
        let deadline = crate::clock::Instant::now() + crate::clock::Duration::from_secs(3);
        loop {
            let acknowledged = store.0.borrow().open[&checkout.id]
                .autosave
                .as_ref()
                .unwrap()
                .durability_snapshot()
                .durable_heads
                == Some(older.clone());
            if acknowledged {
                break;
            }
            assert!(
                crate::clock::Instant::now() < deadline,
                "older writer acknowledgement missing"
            );
            std::thread::sleep(crate::clock::Duration::from_millis(10));
        }
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::Saving
        );
        assert_eq!(
            store
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            pending
        );
        store.try_flush().unwrap();
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::Saved
        );
        drop(reservation);
        drop(store);
        drop(owners);
    }

    #[test]
    fn save_status_returns_busy_during_record_loan() {
        let store = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        assert!(store.open_record(&checkout));
        store
            .with_action_state(&checkout.id, |_, _, _| {
                assert!(matches!(
                    store.save_status(&checkout.id),
                    Err(RecordOpenError::Busy)
                ));
            })
            .unwrap();
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::MemoryOnly
        );
    }

    #[test]
    fn canonical_startup_failure_retains_reservation_and_stale_drop_is_inert() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root);
        let store = owners.open("dev.tilo.test", "room-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut token = store.reserve_record(&want).unwrap();
        let generation = token.generation;
        let owner = store.0.borrow().pending[&want.id].store.clone().unwrap();
        let failure = token.initialize_with_spawn(
            |doc| {
                put_text(doc, "discard", "private");
                Ok(())
            },
            |_| {
                Err(app_state::StoreError::Io {
                    context: "injected initial spawn failure".into(),
                    source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
                })
            },
        );
        assert!(matches!(failure, Err(RecordOpenError::Store(_))));
        assert!(store.open_records().is_empty());
        assert!(!owner.document_path().exists());
        {
            let set = store.0.borrow();
            let reserved = &set.pending[&want.id];
            assert_eq!(reserved.generation, generation);
            assert!(std::sync::Arc::ptr_eq(
                &owner,
                reserved.store.as_ref().unwrap()
            ));
        }
        assert!(app_state::DocStore::open_canonical_at("dev.tilo.test", owner.dir()).is_err());
        token
            .initialize(|doc| {
                assert!(doc.get(ROOT, "discard").unwrap().is_none());
                Ok(())
            })
            .unwrap();
        assert!(
            token
                .initialize(|_| panic!("second initializer must not run"))
                .is_err()
        );
        drop(token);
        assert!(store.is_ready(&want.id));
        // Construct a stale-generation witness privately: no public handle can do this.
        let other = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let token = store.reserve_record(&other).unwrap();
        let stale_generation = token.generation;
        drop(token);
        let newer = store.reserve_record(&other).unwrap();
        let stale = RecordReservation {
            store: store.clone(),
            checkout: other.clone(),
            generation: stale_generation,
        };
        drop(stale);
        assert!(matches!(
            store.try_open_record(&other),
            Err(RecordOpenError::Busy)
        ));
        drop(newer);
        assert!(matches!(
            store.try_open_record(&other),
            Err(RecordOpenError::MissingRecord)
        ));
    }

    #[test]
    fn canonical_repeated_handles_do_not_duplicate_writes_or_rebase_history() {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("dev.tilo.test", "room-a").unwrap();
        let second = owners.open("dev.tilo.test", "room-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_flush().unwrap();
        let before = {
            let set = store.0.borrow();
            let p = &set.open[&want.id];
            let saver = p.autosave.as_ref().unwrap();
            (saver.full_writes(), saver.appends(), p.history.revision())
        };
        store.with(&want.id, |doc| put_text(doc, "body", "one edit"));
        store.tick();
        second.tick();
        assert!(matches!(
            second.try_open_record(&want),
            Ok(RecordOpenOutcome::AlreadyOpen)
        ));
        second.try_flush().unwrap();
        let after = {
            let set = store.0.borrow();
            let p = &set.open[&want.id];
            let saver = p.autosave.as_ref().unwrap();
            (saver.full_writes(), saver.appends(), p.history.revision())
        };
        assert_eq!(after, (before.0, before.1 + 1, before.2));
    }

    fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
        let _ = doc.put(&ROOT, key, value);
    }

    fn text(doc: &AutoCommit, key: &str) -> Option<String> {
        str_at(doc, &ROOT, key)
    }

    #[test]
    fn a_file_record_is_identified_by_its_path() {
        #[cfg(windows)]
        let (root, expected) = ("C:/work", r"file:C:\work\report.odt");
        #[cfg(not(windows))]
        let (root, expected) = ("/work", "file:/work/report.odt");
        let path = Path::new(root).join("report.odt");
        let id = RecordId::for_file(&path);
        assert_eq!(id.as_str(), expected);
        assert_eq!(id.file_path().unwrap(), path);
        assert_eq!(RecordId::parse(expected), Some(id.clone()));
        // Same file, same record, including lexical path aliases.
        assert_eq!(id, RecordId::for_file(Path::new(root).join("./report.odt")));
        assert_eq!(
            id,
            RecordId::for_file(Path::new(root).join("drafts/../report.odt"))
        );
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
    fn room_discovery_is_metadata_only_and_tracks_remote_grants() {
        let mut remote_room = AutoCommit::new();
        let remote = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        assert!(room::reference(&mut remote_room, &remote.id, &remote.kind));

        // Simulate a collaborator's room-index snapshot arriving before this
        // process starts. Learning it must not manufacture a record document.
        let mut local_room = AutoCommit::new();
        local_room
            .merge(&mut remote_room)
            .expect("room indexes merge like other collaboration documents");
        let mut set = RecordSet::in_memory().with_home("room-a");
        assert_eq!(set.discover_room(&local_room), vec![remote.clone()]);
        assert_eq!(set.known_records(), vec![remote.clone()]);
        assert!(
            set.open_records().is_empty(),
            "discovery does not open replicas"
        );

        assert!(room::unreference(&mut local_room, &remote.id));
        assert!(set.discover_room(&local_room).is_empty());
        assert!(
            set.known_records().is_empty(),
            "revocation removes future discovery"
        );
        assert!(set.open_records().is_empty());
    }

    #[test]
    fn derived_file_record_ids_are_valid_room_names() {
        let file = RecordId::for_file("/tmp/shared/report.pdf");
        let derived = RecordId::derived("annotations", &file);
        assert!(RecordId::parse(derived.as_str()).is_some(), "{derived}");
    }

    #[test]
    fn room_discovery_rejects_hostile_entries_and_is_bounded() {
        let mut room_doc = AutoCommit::new();
        let idx = room_doc
            .put_object(&ROOT, INDEX, ObjType::Map)
            .expect("room index");
        // Not an ID shape the host understands, and not a typed map.
        let _ = room_doc.put(&idx, "../../escape", "annotation-set");
        let _ = room_doc.put_object(&idx, "uuid:not-a-uuid", ObjType::Map);
        for _ in 0..(MAX_DISCOVERED_ROOM_RECORDS + 8) {
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
            assert!(room::reference(&mut room_doc, &checkout.id, &checkout.kind));
        }

        let mut set = RecordSet::in_memory().with_home("room-a");
        let found = set.discover_room(&room_doc);
        assert_eq!(found.len(), MAX_DISCOVERED_ROOM_RECORDS);
        assert!(found.iter().all(RecordCheckout::is_valid));
        assert!(set.open_records().is_empty());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn room_discovery_never_crosses_scopes_after_restart() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        // SAFETY: serialised by this module's persistent-record tests.
        unsafe { std::env::set_var("XDG_DATA_HOME", dir.path()) };
        let package = "dev.tilo.room-discovery";
        let a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let entry = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

        let (mut room_a, mut saver_a) = crate::persist::open_room(package, a);
        assert!(room::reference(&mut room_a.doc, &entry.id, &entry.kind));
        saver_a.request(&mut room_a.doc);
        saver_a.flush();

        let (room_b, _) = crate::persist::open_room(package, b);
        let mut set_b = RecordSet::open(package).with_home(b);
        assert!(set_b.discover_room(&room_b.doc).is_empty());

        let (room_a_restarted, _) = crate::persist::open_room(package, a);
        let mut set_a = RecordSet::open(package).with_home(a);
        assert_eq!(set_a.discover_room(&room_a_restarted.doc), vec![entry]);
        assert!(set_a.open_records().is_empty());
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
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
    #[serial_test::serial(state_dir_env)]
    fn history_existing_legacy_record_reopens_without_initializing_missing_content() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: all environment changes in this test use state_dir_env.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let checkout = RecordCheckout::new(RecordId::for_tab(401), RecordKind::new("tab"));
        let package = "dev.qos.history-existing-legacy";
        let room = "11111111-1111-4111-8111-111111111111";
        let original_heads = {
            let store = RecordStore::new(RecordSet::open(package).with_home(room));
            store.open_record(&checkout);
            let heads = store
                .with(&checkout.id, |doc| {
                    put_text(doc, "body", "preserved record");
                    doc.get_heads()
                })
                .unwrap();
            store.flush();
            heads
        };
        let store = RecordStore::new(RecordSet::open(package).with_home(room));
        assert!(
            store.open_existing_record_checked(&checkout).is_ok(),
            "existing-only presentation must load a saved legacy record"
        );
        store
            .with(&checkout.id, |doc| {
                assert_eq!(doc.get_heads(), original_heads);
                assert_eq!(text(doc, "body").as_deref(), Some("preserved record"));
            })
            .unwrap();
        let missing = RecordCheckout::new(RecordId::for_tab(402), RecordKind::new("tab"));
        assert!(matches!(
            store.open_existing_record_checked(&missing),
            Err(RecordOpenError::MissingRecord)
        ));
        assert!(store.read(&missing.id, |_| ()).is_none());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(state_dir_env)]
    fn history_existing_package_record_migrates_once_without_losing_old_history() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: serialized on state_dir_env.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let package = "dev.qos.history-package-record";
        let checkout = RecordCheckout::new(RecordId::for_tab(403), RecordKind::new("tab"));
        let room = "11111111-1111-4111-8111-111111111111";
        let old_heads = {
            let mut owner =
                app_state::DocStore::open_record(package, &checkout.id.storage_key()).unwrap();
            let mut doc = owner.new_document();
            init(&mut doc, &checkout.id, &checkout.kind, room);
            put_text(&mut doc, "body", "old package content");
            let heads = doc.get_heads();
            owner.save(&mut doc).unwrap();
            heads
        };
        let legacy_path =
            app_state::DocStore::legacy_record_document_path(package, &checkout.id.storage_key())
                .unwrap();
        let original_bytes = std::fs::read(&legacy_path).unwrap();
        {
            let store = RecordStore::new(RecordSet::open(package).with_home(room));
            assert!(
                store.open_existing_record_checked(&checkout).is_ok(),
                "an existing package record must remain readable"
            );
            store
                .with(&checkout.id, |doc| {
                    assert_eq!(text(doc, "body").as_deref(), Some("old package content"));
                    for head in &old_heads {
                        assert!(doc.get_change_by_hash(head).is_some());
                    }
                    assert_eq!(room_scope(doc).as_deref(), Some(room));
                })
                .unwrap();
            store.flush();
        }
        assert_eq!(std::fs::read(&legacy_path).unwrap(), original_bytes);
        assert!(
            !app_state::DocStore::claim_legacy_record_migration(
                package,
                "22222222-2222-4222-8222-222222222222",
                &checkout.id.storage_key(),
            )
            .unwrap(),
            "the durable migration claim remains pinned to its original room"
        );
        let other = RecordStore::new(
            RecordSet::open(package).with_home("22222222-2222-4222-8222-222222222222"),
        );
        assert!(
            other.open_existing_record_checked(&checkout).is_err(),
            "migration cannot admit another room"
        );
        let reopened = RecordStore::new(RecordSet::open(package).with_home(room));
        assert!(reopened.open_existing_record_checked(&checkout).is_ok());
        assert_eq!(
            reopened
                .read(&checkout.id, |doc| text(doc, "body"))
                .flatten()
                .as_deref(),
            Some("old package content")
        );
        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(state_dir_env)]
    fn malformed_qos_migration_source_never_claims_a_room() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };
        let room = "11111111-1111-4111-8111-111111111111";
        let other_room = "22222222-2222-4222-8222-222222222222";

        for malformed in ["room", "refs"] {
            let package = format!("dev.qos.malformed-{malformed}");
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
            let key = checkout.id.storage_key();
            let mut owner = app_state::DocStore::open_record(&package, &key).unwrap();
            let mut doc = owner.new_document();
            let metadata = doc.put_object(ROOT, LEGACY_META, ObjType::Map).unwrap();
            doc.put(&metadata, "id", checkout.id.as_str()).unwrap();
            doc.put(&metadata, "kind", checkout.kind.as_str()).unwrap();
            doc.put(&metadata, "home", room).unwrap();
            match malformed {
                "room" => doc.put(&metadata, ROOM, other_room).unwrap(),
                "refs" => doc.put(&metadata, "refs", "not a list").unwrap(),
                _ => unreachable!(),
            }
            owner.save(&mut doc).unwrap();

            let store = RecordStore::new(RecordSet::open(&package).with_home(room));
            assert!(matches!(
                store.open_existing_record_checked(&checkout),
                Err(RecordOpenError::IdentityMismatch)
            ));
            assert!(
                app_state::DocStore::claim_legacy_record_migration(&package, other_room, &key)
                    .unwrap(),
                "a rejected {malformed} source must not leave a durable room claim"
            );
        }

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(state_dir_env)]
    fn rejected_qos_checkout_preserves_the_active_record() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };

        let package = "dev.qos.rejected-checkout";
        let room = "11111111-1111-4111-8111-111111111111";
        let active = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let rejected = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut room_doc = AutoCommit::new();
        let mut set = RecordSet::open(package).with_home(room);
        let mut state = State::new();
        assert!(set.checkout(&mut state, Some(&mut room_doc), &active));
        put_text(&mut state.doc, "body", "keep this content");

        let key = rejected.id.storage_key();
        let mut owner = app_state::DocStore::open_room_record(package, room, &key).unwrap();
        let mut malformed = owner.new_document();
        let metadata = malformed
            .put_object(ROOT, LEGACY_META, ObjType::Map)
            .unwrap();
        malformed
            .put(&metadata, "id", rejected.id.as_str())
            .unwrap();
        malformed
            .put(&metadata, "kind", rejected.kind.as_str())
            .unwrap();
        malformed.put(&metadata, "home", room).unwrap();
        malformed
            .put(&metadata, ROOM, "22222222-2222-4222-8222-222222222222")
            .unwrap();
        owner.save(&mut malformed).unwrap();

        assert!(!set.checkout(&mut state, Some(&mut room_doc), &rejected));
        assert_eq!(set.active.as_ref(), Some(&active.id));
        assert_eq!(
            text(&state.doc, "body").as_deref(),
            Some("keep this content")
        );
        assert!(!set.open.contains_key(&rejected.id));

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    fn history_new_record_creation_refuses_reuse_and_preserves_the_live_graph() {
        let store = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::for_tab(404), RecordKind::new("tab"));
        store.create_record(&checkout).unwrap();
        let before = store
            .with(&checkout.id, |doc| {
                put_text(doc, "body", "already authored");
                doc.get_heads()
            })
            .unwrap();
        assert!(matches!(
            store.create_record(&checkout),
            Err(RecordOpenError::AlreadyExists)
        ));
        assert_eq!(
            store.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            before
        );
    }

    #[test]
    #[serial_test::serial(state_dir_env)]
    fn history_existing_scoped_refuses_corruption_empty_and_mismatched_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: serialized on state_dir_env.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let package = "dev.qos.history-record-refusals";
        let room = "11111111-1111-4111-8111-111111111111";
        for (index, payload) in [
            b"not automerge".to_vec(),
            Vec::new(),
            {
                let mut doc = AutoCommit::new();
                init_scoped(
                    &mut doc,
                    &RecordId::for_tab(999),
                    &RecordKind::new("tab"),
                    room,
                    Some(room),
                );
                doc.save()
            },
            {
                let mut doc = AutoCommit::new();
                init_scoped(
                    &mut doc,
                    &RecordId::for_tab(413),
                    &RecordKind::new("tab"),
                    room,
                    Some(room),
                );
                let mut bytes = doc.save();
                put_text(&mut doc, "body", "truncated later content");
                let increment = doc.save_incremental();
                assert!(increment.len() > 8);
                bytes.extend_from_slice(&increment[..increment.len() - 3]);
                bytes
            },
        ]
        .into_iter()
        .enumerate()
        {
            let checkout = RecordCheckout::new(
                RecordId::for_tab(410 + index as u64),
                RecordKind::new("tab"),
            );
            let key = checkout.id.storage_key();
            // A valid older package source must never hide a damaged scoped file.
            {
                let mut legacy = app_state::DocStore::open_record(package, &key).unwrap();
                let mut doc = legacy.new_document();
                init(&mut doc, &checkout.id, &checkout.kind, room);
                put_text(&mut doc, "body", "must not become a fallback");
                legacy.save(&mut doc).unwrap();
            }
            let path = {
                let owner = app_state::DocStore::open_room_record(package, room, &key).unwrap();
                owner.document_path().to_path_buf()
            };
            std::fs::write(&path, &payload).unwrap();
            let store = RecordStore::new(RecordSet::open(package).with_home(room));
            assert!(store.open_existing_record_checked(&checkout).is_err());
            assert!(store.create_record(&checkout).is_err());
            assert!(store.read(&checkout.id, |_| ()).is_none());
            store.tick();
            store.flush();
            assert_eq!(std::fs::read(&path).unwrap(), payload);
        }
        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    fn history_explicit_canonical_creation_survives_reopen_and_refuses_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let checkout = RecordCheckout::new(RecordId::for_tab(420), RecordKind::new("tab"));
        let heads = {
            let mut owners = RecordOwners::new(root.clone());
            let store = owners.open("dev.qos.test", "history-room").unwrap();
            store.create_record(&checkout).unwrap();
            let heads = store
                .with(&checkout.id, |doc| {
                    put_text(doc, "body", "new canonical content");
                    doc.get_heads()
                })
                .unwrap();
            store.try_flush().unwrap();
            heads
        };
        let mut owners = RecordOwners::new(root);
        let store = owners.open("dev.qos.test", "history-room").unwrap();
        assert!(store.create_record(&checkout).is_err());
        store.open_existing_record_checked(&checkout).unwrap();
        assert_eq!(
            store.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            heads
        );
        assert_eq!(
            store
                .read(&checkout.id, |doc| text(doc, "body"))
                .flatten()
                .as_deref(),
            Some("new canonical content")
        );
    }

    #[test]
    fn history_new_record_creation_reports_busy_during_an_existing_record_loan() {
        let store = RecordStore::in_memory();
        let existing = RecordCheckout::new(RecordId::for_tab(421), RecordKind::new("tab"));
        let new = RecordCheckout::new(RecordId::for_tab(422), RecordKind::new("tab"));
        store.create_record(&existing).unwrap();
        let result = store
            .with(&existing.id, |_| store.create_record(&new))
            .unwrap();
        assert!(matches!(result, Err(RecordOpenError::Busy)));
        assert!(store.read(&new.id, |_| ()).is_none());
    }

    #[test]
    fn history_new_canonical_record_creation_reports_busy_during_read_loan() {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("dev.qos.test", "read-loan").unwrap();
        let existing = RecordCheckout::new(RecordId::for_tab(423), RecordKind::new("tab"));
        let new = RecordCheckout::new(RecordId::for_tab(424), RecordKind::new("tab"));
        store.create_record(&existing).unwrap();
        let result = store
            .read(&existing.id, |_| store.create_record(&new))
            .unwrap();
        assert!(matches!(result, Err(RecordOpenError::Busy)));
        assert!(store.read(&new.id, |_| ()).is_none());
    }

    #[test]
    // Shares a serial key with every other test mutating the process-global
    // `XDG_DATA_HOME` (see `persist::tests`), so they don't race on it.
    #[serial_test::serial(process_env)]
    fn each_record_persists_to_its_own_file_and_comes_back() {
        let tmp = tempfile::tempdir().expect("temp dir");
        // SAFETY: this test is serialised on `process_env`; no other
        // thread reads the variable between the set and the reads below.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
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
            let mut set = RecordSet::open("dev.tilo.test-records").with_home(room_id);
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
        let mut set = RecordSet::open("dev.tilo.test-records").with_home(room_id);
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
            .join("dev.tilo.test-records")
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
    #[serial_test::serial(process_env)]
    fn same_record_id_in_two_rooms_never_aliases_and_each_hydrates_after_restart() {
        let tmp = tempfile::tempdir().expect("temp dir");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }
        let a = "22222222-2222-4222-8222-222222222222";
        let b = "33333333-3333-4333-8333-333333333333";
        let record = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

        for (room, body) in [(a, "private to a"), (b, "private to b")] {
            let mut set = RecordSet::open("dev.tilo.scoped-records").with_home(room);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &record));
            put_text(&mut state.doc, "body", body);
            set.flush(&mut state.doc);
        }

        for (room, expected) in [(a, "private to a"), (b, "private to b")] {
            let mut set = RecordSet::open("dev.tilo.scoped-records").with_home(room);
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

#[cfg(test)]
mod portable_tests {
    use super::*;

    fn qos_record(id: &RecordId, kind: &RecordKind, home: &str, room: Option<&str>) -> AutoCommit {
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![7; 32]));
        doc.put(
            ROOT,
            app_state::SCHEMA_VERSION_KEY,
            u64::from(app_state::SCHEMA_VERSION),
        )
        .unwrap();
        let meta = doc.put_object(ROOT, "qos:record", ObjType::Map).unwrap();
        doc.put(&meta, "id", id.as_str()).unwrap();
        doc.put(&meta, "kind", kind.as_str()).unwrap();
        doc.put(&meta, "home", home).unwrap();
        if let Some(room) = room {
            doc.put(&meta, ROOM, room).unwrap();
        }
        doc.put_object(&meta, "refs", ObjType::List).unwrap();
        doc.put(ROOT, "body", "deployed qOS bytes").unwrap();
        doc.commit();
        doc
    }

    #[test]
    fn portable_store_reopens_and_updates_qos_metadata_without_renaming_it() {
        let cache =
            crate::portable_storage::PortableStorage::new("legacy-profile", "device").unwrap();
        let package = "dev.qos.notes";
        let workspace = "legacy-room";
        let checkout = RecordCheckout::new(RecordId::for_tab(9), RecordKind::new("notes.notebook"));
        let mut frozen = qos_record(&checkout.id, &checkout.kind, workspace, Some(workspace));
        let original_heads = frozen.base_heads().to_vec();
        let actor = frozen.get_actor().to_bytes().to_vec();
        cache
            .hydrate(
                StorageKey::Record {
                    package: package.into(),
                    workspace: workspace.into(),
                    record: checkout.id.as_str().into(),
                },
                frozen.save(),
                Some(actor),
            )
            .unwrap();

        let store = RecordOwners::portable(cache.clone())
            .open(package, workspace)
            .unwrap();
        store.try_open_record(&checkout).unwrap();
        store.read(&checkout.id, |doc| {
            assert_eq!(doc.base_heads(), original_heads);
            assert_eq!(
                str_at(doc, &doc.get(ROOT, "qos:record").unwrap().unwrap().1, "id").as_deref(),
                Some(checkout.id.as_str())
            );
            assert!(doc.get(ROOT, "tilo:record").unwrap().is_none());
            assert_eq!(
                str_at(doc, &ROOT, "body").as_deref(),
                Some("deployed qOS bytes")
            );
        });

        let referenced = RecordRef::new(RecordId::for_tab(10), "attached-to");
        assert_eq!(
            store.with(&checkout.id, |doc| add_reference(doc, &referenced)),
            Some(true)
        );
        store.read(&checkout.id, |doc| {
            assert_eq!(references(doc), vec![referenced]);
            assert!(doc.get(ROOT, "qos:record").unwrap().is_some());
            assert!(doc.get(ROOT, "tilo:record").unwrap().is_none());
        });

        let rejected =
            RecordCheckout::new(RecordId::for_tab(13), RecordKind::new("notes.notebook"));
        let mut malformed = qos_record(&rejected.id, &rejected.kind, workspace, Some(workspace));
        let metadata = malformed.get(ROOT, LEGACY_META).unwrap().unwrap().1;
        malformed.put(&metadata, "refs", "not a list").unwrap();
        malformed.commit();
        let actor = malformed.get_actor().to_bytes().to_vec();
        cache
            .hydrate(
                StorageKey::Record {
                    package: package.into(),
                    workspace: workspace.into(),
                    record: rejected.id.as_str().into(),
                },
                malformed.save(),
                Some(actor),
            )
            .unwrap();
        assert!(matches!(
            store.try_open_record(&rejected),
            Err(RecordOpenError::IdentityMismatch)
        ));
    }

    #[test]
    fn qos_metadata_cannot_cross_room_pass_as_primary_or_mix_with_tilo() {
        let id = RecordId::for_tab(12);
        let kind = RecordKind::new("note");
        let mut roomless = qos_record(&id, &kind, "room-a", None);
        assert!(init_scoped(
            &mut roomless,
            &id,
            &kind,
            "room-a",
            Some("room-a")
        ));
        assert_eq!(room_scope(&roomless).as_deref(), Some("room-a"));
        assert!(roomless.get(ROOT, "tilo:record").unwrap().is_none());

        let mut wrong_room = qos_record(&id, &kind, "room-a", Some("room-a"));
        assert!(!init_scoped(
            &mut wrong_room,
            &id,
            &kind,
            "room-b",
            Some("room-b")
        ));
        assert!(wrong_room.get(ROOT, "tilo:record").unwrap().is_none());
        assert!(matches!(
            validate_identity(
                &wrong_room,
                &RecordCheckout::package_primary(RecordKind::new("primary")),
                "room-a"
            ),
            Err(RecordOpenError::IdentityMismatch)
        ));

        let mut mixed = qos_record(&id, &kind, "room-a", Some("room-a"));
        let current = mixed.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
        mixed.put(&current, "id", id.as_str()).unwrap();
        mixed.put(&current, "kind", kind.as_str()).unwrap();
        assert!(identity(&mixed).is_none());
        assert!(!init_scoped(
            &mut mixed,
            &id,
            &kind,
            "room-a",
            Some("room-a")
        ));

        let mut malformed = AutoCommit::new();
        malformed.put(ROOT, "qos:record", "not a map").unwrap();
        assert!(!init_scoped(
            &mut malformed,
            &id,
            &kind,
            "room-a",
            Some("room-a")
        ));
        assert!(malformed.get(ROOT, "tilo:record").unwrap().is_none());

        let mut base = qos_record(&id, &kind, "room-a", None);
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).unwrap();
        left.set_actor(automerge::ActorId::from(vec![8; 32]));
        let left_meta = left.get(ROOT, "qos:record").unwrap().unwrap().1;
        left.put(&left_meta, ROOM, "room-a").unwrap();
        let mut right = AutoCommit::load(&bytes).unwrap();
        right.set_actor(automerge::ActorId::from(vec![9; 32]));
        let right_meta = right.get(ROOT, "qos:record").unwrap().unwrap().1;
        right.put(&right_meta, ROOM, "room-b").unwrap();
        left.merge(&mut right).unwrap();
        assert_eq!(left.get_all(&left_meta, ROOM).unwrap().len(), 2);
        assert!(!init_scoped(
            &mut left,
            &id,
            &kind,
            "room-a",
            Some("room-a")
        ));
    }

    #[test]
    fn qos_room_index_is_read_and_updated_in_its_original_namespace() {
        let mut doc = AutoCommit::new();
        let index = doc.put_object(ROOT, "qos:records", ObjType::Map).unwrap();
        let first = RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("note"));
        let entry = doc
            .put_object(&index, first.id.as_str(), ObjType::Map)
            .unwrap();
        doc.put(&entry, "kind", first.kind.as_str()).unwrap();
        assert_eq!(room::references(&doc), vec![first.clone()]);
        let updated_kind = RecordKind::new("updated-note");
        assert!(!room::reference(&mut doc, &first.id, &updated_kind));
        assert_eq!(room::references(&doc)[0].kind, updated_kind);

        let second = RecordCheckout::new(RecordId::for_tab(2), RecordKind::new("note"));
        assert!(room::reference(&mut doc, &second.id, &second.kind));
        assert!(room::holds(&doc, &second.id));
        assert!(doc.get(ROOT, "qos:records").unwrap().is_some());
        assert!(doc.get(ROOT, "tilo:records").unwrap().is_none());

        let duplicate = doc.put_object(ROOT, "tilo:records", ObjType::Map).unwrap();
        doc.put_object(&duplicate, RecordId::for_tab(3).as_str(), ObjType::Map)
            .unwrap();
        assert!(room::references(&doc).is_empty());
        assert!(!room::reference(
            &mut doc,
            &RecordId::for_tab(4),
            &RecordKind::new("note")
        ));
    }

    #[test]
    fn portable_owner_preserves_reservation_identity_actor_and_durability() {
        let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
        let mut owners = RecordOwners::portable(cache.clone());
        let store = owners.open("dev.qos.test", "room").unwrap();
        let again = owners.open("dev.qos.test", "room").unwrap();
        assert!(store.same_owner(&again));
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut reservation = store.reserve_record(&checkout).unwrap();
        assert!(again.reserve_record(&checkout).is_err());
        assert!(!store.is_ready(&checkout.id));
        reservation
            .initialize(|doc| {
                doc.put(automerge::ROOT, "title", "Hello").unwrap();
                Ok(())
            })
            .unwrap();
        let actor = store
            .read(&checkout.id, |doc| doc.get_actor().clone())
            .unwrap();
        assert!(store.try_flush().is_err());
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        assert!(store.try_flush().is_ok());
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::Saved
        );
        drop(reservation);
        drop(again);
        drop(store);
        let reopened = owners.open("dev.qos.test", "room").unwrap();
        reopened.try_open_record(&checkout).unwrap();
        assert_eq!(
            reopened
                .read(&checkout.id, |doc| doc.get_actor().clone())
                .unwrap(),
            actor
        );
        let wrong_kind = RecordCheckout::new(checkout.id.clone(), RecordKind::new("other"));
        assert!(matches!(
            reopened.try_open_record(&wrong_kind),
            Err(RecordOpenError::IdentityMismatch)
        ));
    }
    #[test]
    fn portable_failed_initializer_keeps_reservation_until_drop() {
        let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
        let store = RecordOwners::portable(cache.clone())
            .open("dev.qos.test", "room")
            .unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut reservation = store.reserve_record(&checkout).unwrap();
        assert!(reservation.initialize(|_| Err("failed".into())).is_err());
        assert!(store.reserve_record(&checkout).is_err());
        assert!(cache.pending_writes().is_empty());
        drop(reservation);
        assert!(store.reserve_record(&checkout).is_ok());
    }
}
