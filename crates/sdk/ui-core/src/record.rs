//! Records — the unit of state, collaboration and storage: a type, a stable identity and a home
//! workspace. A file's identity is its file record, which a rename or move keeps, and its path is
//! only where it sits; records reference other records rather than nesting, so "share the book
//! without my notes" is expressible. One document per record is forced, not chosen: an Automerge
//! change graph names its dependencies by hash, so a filtered stream leaves orphaned changes.

pub mod bootstrap;
/// A record's identity, as it travels on the collaboration wire.
pub use ::collaboration::record::{
    MAX_FORMER_IDS, MAX_RECORD_RELATION_BYTES, RecordCheckout, RecordId, RecordKind, RecordRef,
    RecordSaveStatus, valid_label,
};
pub mod capability;
use std::collections::{BTreeMap, HashMap, HashSet};
use tilo_sort::CompactSort as _;

use crate::portable_storage::PortableStorage;
mod personal_scope;
use personal_scope::PersonScope;
mod portable;
use std::path::PathBuf;
use std::rc::Rc;
use tilo_automerge::ApplyIncoming;

use durable_store::{DurableStore, StorageKey};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

mod history_access;
pub use history_access::HistoryUnavailable;
pub mod person;
pub use person::StoredRecord;
pub mod sync;
pub use sync::{Outbound, RecordSync};
pub mod transfer;
pub mod workspace;

use ::collaboration::record::fields::{META, ObjectAt, StringAt, meta_ro, object_at, string_at};
/// Key in a record document for the workspace that owns this local replica. It keeps the name
/// records were first written under: peers may run a build from before this module named the field
/// for what it means.
const WORKSPACE: &str = "room";
/// The key a record written while the field was named `workspace` carries its workspace under;
/// those documents exist, so it is read where it is (see [`stored_workspace`]) and never written.
pub const WORKSPACE_ALIAS: &str = "workspace";
/// Key, in a workspace's index document, of the map of record references.
const INDEX: &str = "tilo:records";
/// Key, in a record's meta map, of the identities it was adopted from.
const FORMERLY: &str = "formerly";

/// Upper bound for the records an app learns from one workspace index at once: the index is
/// replicated input, not a request to allocate one local document per entry.
pub const MAX_DISCOVERED_RECORDS: usize = 64;
const MAX_INDEX_KEYS_SCANNED: usize = MAX_DISCOVERED_RECORDS * 4;

/// Select the document's metadata map, or create it only when it is absent.
fn meta_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => doc.put_object(&ROOT, META, ObjType::Map).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match string_at(doc, obj, key) {
        StringAt::String(value) => Some(value),
        StringAt::Missing | StringAt::Invalid => None,
    }
}

/// The workspace a record document is stamped with: the key every record is written under, or the
/// one a build used while the field was named `workspace`. A value that is not a name cannot say
/// which workspace it belongs to, and is refused.
fn stored_workspace(doc: &AutoCommit, meta: &ObjId) -> StringAt {
    match string_at(doc, meta, WORKSPACE) {
        StringAt::Missing => string_at(doc, meta, WORKSPACE_ALIAS),
        written => written,
    }
}

/// Stamp a document with the identity of the record it holds; idempotent, so a reopen does not
/// author a change graph entry that says nothing new.
pub fn init(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind, home: &str) {
    let _ = init_scoped(doc, id, kind, home, None);
}

/// Bind a record document to `workspace`, never rewriting an existing identity or workspace: a
/// document stamped for another workspace would turn a matching record id into an alias across
/// workspaces. A document naming none may be adopted once, after its identity is checked.
pub fn init_scoped(
    doc: &mut AutoCommit,
    id: &RecordId,
    kind: &RecordKind,
    home: &str,
    workspace: Option<&str>,
) -> bool {
    // Primary custody is the checked package/workspace namespace, never a fake
    // record stamp. Keep this serialized internal id stable across hosts.
    if id.is_package_primary() {
        return primary_document(doc);
    }
    if RecordId::parse(id.as_str()).is_none() || !kind.is_valid() {
        return false;
    }
    // A stamp written on its own is the record's genesis and is committed as one; with other
    // pending operations it is part of the change those make, which names itself.
    let alone = doc.pending_ops() == 0;
    let stamped = stamp_scoped(doc, id, kind, home, workspace);
    if alone && stamped && doc.pending_ops() != 0 {
        commit_genesis(doc);
    } else if alone && stamped && stamp_surface(doc).is_some() {
        // The app's package opens a record its surface did not name yet (a newer package, or a
        // record from before records named one): its surface decides the record's changes from here
        // on.
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
        );
    }
    stamped
}

fn stamp_scoped(
    doc: &mut AutoCommit,
    id: &RecordId,
    kind: &RecordKind,
    home: &str,
    workspace: Option<&str>,
) -> bool {
    let Some(m) = meta_obj(doc) else {
        // The document's root object is gone — nothing to stamp.
        return false;
    };
    let existing_id = string_at(doc, &m, "id");
    let existing_kind = string_at(doc, &m, "kind");
    let existing_home = string_at(doc, &m, "home");
    let existing_workspace = stored_workspace(doc, &m);
    let existing_refs = object_at(doc, &m, "refs", ObjType::List);
    if matches!(&existing_id, StringAt::String(existing) if existing != id.as_str())
        || matches!(&existing_id, StringAt::Invalid)
        || matches!(&existing_kind, StringAt::String(existing) if !kind.names(existing))
        || matches!(&existing_kind, StringAt::Invalid)
        || matches!(&existing_home, StringAt::String(existing) if existing.is_empty())
        || matches!(&existing_home, StringAt::Invalid)
        || matches!(&existing_workspace, StringAt::Invalid)
        || workspace.is_some_and(
            |scope| matches!(&existing_workspace, StringAt::String(existing) if existing != scope),
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
    // Home is the workspace that minted the record; never rewritten on a
    // later open, or the last workspace to open it would become the owner.
    if matches!(&existing_home, StringAt::Missing) {
        let _ = doc.put(&m, "home", home);
    }
    // A document that already names a workspace under either key keeps it: nothing here rewrites a
    // record's workspace, or moves it to the key this build writes.
    if let Some(workspace) = workspace
        && matches!(&existing_workspace, StringAt::Missing)
    {
        let _ = doc.put(&m, WORKSPACE, workspace);
    }
    if matches!(&existing_refs, ObjectAt::Missing) {
        let _ = doc.put_object(&m, "refs", ObjType::List);
    }
    true
}

/// Commit what a brand-new record starts with (its identity, references and
/// first content) as its genesis, which names itself as such.
fn commit_genesis(doc: &mut AutoCommit) {
    let _ = stamp_surface(doc);
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
}

/// The key of the record identity naming the surface its changes are
/// decided against: the content hash of its app package's `surface.toml`.
pub const SURFACE: &str = "surface";

/// Name, in the record's identity, the surface of its app running here: `Some(true)` when it
/// replaced another app surface, `Some(false)` when it named the first, `None` when nothing
/// changed.
fn stamp_surface(doc: &mut AutoCommit) -> Option<bool> {
    let m = meta_ro(doc)?;
    let kind = str_at(doc, &m, "kind")?;
    let hash = hex::encode(crate::app_surface::hash_of_kind(&kind)?);
    match str_at(doc, &m, SURFACE) {
        Some(named) if named == hash => None,
        named => {
            doc.put(&m, SURFACE, hash).ok()?;
            Some(named.is_some())
        }
    }
}

/// The identity stamped into `doc`, if it holds a record.
pub fn identity(doc: &AutoCommit) -> Option<RecordCheckout> {
    let m = meta_ro(doc)?;
    let id = RecordId::parse(&str_at(doc, &m, "id")?)?;
    let kind = RecordKind::new(str_at(doc, &m, "kind")?);
    let checkout = RecordCheckout::new(id, kind);
    checkout.is_valid().then_some(checkout)
}

/// Identities this record's document was known by before adoption, oldest
/// first (see [`RecordCheckout::formerly`]).
pub fn former_identities(doc: &AutoCommit) -> Vec<RecordId> {
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&m, FORMERLY) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| match doc.get(&list, i) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => RecordId::parse(s),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Give an adopted document `want`'s identity: the stamped id becomes the new one, the old is
/// remembered, and references to a former identity of a declared reference target now name that
/// target. Untouched when it isn't `former`'s document of `want`'s kind.
fn restamp(doc: &mut AutoCommit, former: &RecordId, want: &RecordCheckout) -> bool {
    if identity(doc).is_none_or(|found| found.id != *former || found.kind != want.kind) {
        return false;
    }
    let Some(m) = meta_obj(doc) else {
        return false;
    };
    let _ = doc.put(&m, "id", want.id.as_str());
    let list = match doc.get(&m, FORMERLY) {
        Ok(Some((Value::Object(ObjType::List), list))) => Some(list),
        _ => doc.put_object(&m, FORMERLY, ObjType::List).ok(),
    };
    if let Some(list) = list {
        let n = doc.length(&list);
        let _ = doc.insert(&list, n, former.as_str());
    }
    let Some(refs) = refs_obj(doc) else {
        return true;
    };
    for i in (0..doc.length(&refs)).rev() {
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&refs, i) else {
            continue;
        };
        let (Some(target), Some(relation)) = (
            str_at(doc, &entry, "target"),
            str_at(doc, &entry, "relation"),
        ) else {
            continue;
        };
        let Some((successor, _)) = want
            .references
            .iter()
            .find(|(declared, declared_relation)| {
                *declared_relation == relation
                    && declared.formerly.iter().any(|f| f.as_str() == target)
            })
        else {
            continue;
        };
        let successor_ref = RecordRef::new(successor.id.clone(), relation);
        if references(doc).contains(&successor_ref) {
            let _ = doc.delete(&refs, i);
        } else {
            let _ = doc.put(&entry, "target", successor.id.as_str());
        }
    }
    true
}

/// Adopt the stored document of the first former identity of `want` that exists, belongs to its
/// kind and workspace, and whose custody this successor can claim. The former document is only
/// read: the copy is restamped and returned to persist under the new id.
fn adopt_former(
    store: &Rc<dyn DurableStore>,
    want: &RecordCheckout,
    workspace: &str,
    key: impl Fn(&RecordId) -> StorageKey,
    mut load: impl FnMut(&StorageKey) -> Result<Option<AutoCommit>, app_state::StoreError>,
) -> Result<Option<AutoCommit>, app_state::StoreError> {
    for former in &want.formerly {
        let former_key = key(former);
        if store
            .read(&former_key)?
            .is_none_or(|stored| stored.bytes.is_empty())
        {
            continue;
        }
        let Some(mut doc) = load(&former_key)? else {
            continue;
        };
        if workspace_scope(&doc).is_some_and(|scope| scope != workspace)
            || identity(&doc).is_none_or(|found| found.id != *former || found.kind != want.kind)
        {
            tracing::warn!(record = %want.id, former = %former, "former record does not match; not adopting it");
            continue;
        }
        // First successor wins, durably: another record naming the same former identity later must
        // never receive these edits as well.
        if !store.claim(&former_key, &want.id.storage_key())? {
            tracing::info!(record = %want.id, former = %former, "former record already adopted by another record");
            continue;
        }
        if !restamp(&mut doc, former, want) {
            continue;
        }
        tracing::info!(record = %want.id, former = %former, workspace, "adopting a record under its new identity");
        return Ok(Some(doc));
    }
    Ok(None)
}

/// The workspace this record was minted in.
pub fn home(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    str_at(doc, &m, "home")
}

/// The workspace this local replica is bound to, under either key, absent only for records written
/// before records were scoped to a workspace; callers may adopt those once with [`init_scoped`],
/// but must never treat an explicit other workspace as interchangeable.
pub fn workspace_scope(doc: &AutoCommit) -> Option<String> {
    let m = meta_ro(doc)?;
    match stored_workspace(doc, &m) {
        StringAt::String(workspace) => Some(workspace),
        StringAt::Missing | StringAt::Invalid => None,
    }
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

/// A content-addressed blob a record depends on: immutable bytes its content is about or embeds
/// (the PDF an annotation set marks up, an image a document shows). Kept in the record's metadata,
/// so sync, retention and history see which blobs a record keeps alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    /// Lowercase hex BLAKE3 of the bytes — the blob store's key.
    pub hash: String,
    pub len: u64,
    pub mime: String,
    /// What the blob is to the record — `source`, `embedded`, `import`.
    pub relation: String,
}

fn valid_blob_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn blobs_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    let m = meta_obj(doc)?;
    match doc.get(&m, "blobs") {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => doc.put_object(&m, "blobs", ObjType::List).ok(),
    }
}

/// Record that this record depends on `blob`. Idempotent on
/// `(hash, relation)`. Returns whether a reference was added.
pub fn add_blob_reference(doc: &mut AutoCommit, blob: &BlobRef) -> bool {
    if !valid_blob_hash(&blob.hash)
        || !valid_label(&blob.relation, MAX_RECORD_RELATION_BYTES)
        || blob.mime.len() > MAX_RECORD_RELATION_BYTES
    {
        return false;
    }
    if blob_references(doc)
        .iter()
        .any(|b| b.hash == blob.hash && b.relation == blob.relation)
    {
        return false;
    }
    let Some(list) = blobs_obj(doc) else {
        return false;
    };
    let n = doc.length(&list);
    let Ok(entry) = doc.insert_object(&list, n, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&entry, "hash", blob.hash.as_str());
    let _ = doc.put(&entry, "len", i64::try_from(blob.len).unwrap_or(i64::MAX));
    let _ = doc.put(&entry, "mime", blob.mime.as_str());
    let _ = doc.put(&entry, "relation", blob.relation.as_str());
    true
}

/// Every blob this record depends on.
pub fn blob_references(doc: &AutoCommit) -> Vec<BlobRef> {
    let Some(m) = meta_ro(doc) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&m, "blobs") else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, i) else {
                return None;
            };
            let hash = str_at(doc, &entry, "hash").filter(|h| valid_blob_hash(h))?;
            let len = match doc.get(&entry, "len") {
                Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                    ScalarValue::Int(n) => u64::try_from(*n).ok(),
                    ScalarValue::Uint(n) => Some(*n),
                    _ => None,
                },
                _ => None,
            }?;
            Some(BlobRef {
                hash,
                len,
                mime: str_at(doc, &entry, "mime").unwrap_or_default(),
                relation: str_at(doc, &entry, "relation")?,
            })
        })
        .collect()
}

// The record index: which records a workspace holds.

/// A workspace's index of the records it holds. Ownership of the document is the caller's business.
/// **The reference is what grants access**: a member reaches only the records the workspace
/// references, and removing one stops future changes but cannot un-share bytes a peer already
/// synced.
pub mod index;

/// One record this process has open but is not currently showing.
struct Parked {
    /// `None` exactly while checked out — the live document is the
    /// toolkit's, not this one's.
    doc: Option<AutoCommit>,
    kind: RecordKind,
    /// Debounced writer for this record's file. `None` for an in-memory
    /// set, or when the store failed to open (reported once, not per frame).
    autosave: Option<app_state::DocWriter>,
    /// Belongs to the record: an undo entry is a pair of change hashes in *this* document's graph,
    /// so sharing a stack would revert the wrong history.
    history: crate::state::UndoLog,
    action_state: crate::bound_dispatch::RecordActionState,
}

/// The records one app process has open, and which is live. Owns per-record persistence and
/// parking, not collaboration: routing a record's deltas is the toolkit's business, since a channel
/// belongs to the connection.
pub struct RecordSet {
    package_id: String,
    /// The workspace these records are opened in, which is the cluster its
    /// route names. Stamped as a record's home when it is first created here.
    home: String,
    /// The person's own cluster, for records kept in their own scope: their route names it, so one
    /// record of an app is one record on every device they use, in their store rather than a
    /// workspace's.
    person: Option<uuid::Uuid>,
    /// The workspace this person's records were kept in before their own store existed: a record
    /// whose own key holds nothing is read here once and taken into their scope.
    retired: Option<String>,
    open: HashMap<RecordId, Parked>,
    /// Names currently granted by the workspace index. Discovery is separate from `open`: a hostile
    /// or merely large index must not create local replicas or subscribe the app to unrelated
    /// documents.
    known: BTreeMap<RecordId, RecordKind>,
    active: Option<RecordId>,
    /// False for [`RecordSet::in_memory`]: no per-record files are opened, so
    /// a test drives the same checkout path without touching the disk.
    persistent: bool,
    /// Where the launch-environment route keeps its documents: shared
    /// writers on package-scoped keys. `None` for other sets.
    documents: Option<Rc<dyn DurableStore>>,
    /// The host's storage, for a set opened through [`RecordOwners`]. `None`
    /// for the launch-environment route and for [`RecordSet::in_memory`].
    backend: Option<Rc<dyn DurableStore>>,
    /// A hydrated, host-acknowledged browser cache (Cloud portable sessions).
    portable: Option<PortableStorage>,
    pending: HashMap<RecordId, Reserved>,
    /// Records being opened off the frame ([`RecordStore::begin_opening`]): not open until they
    /// are taken in.
    opening: HashMap<RecordId, opening::Opening>,
    /// The records lent out to an action frame: their documents are with the frame, so the other
    /// records stay open to it meanwhile.
    lent: HashSet<RecordId>,
    next_reservation: u64,
    // Retained from the private native construction scope; weakly borrows the
    // one actual worker after construction ends. Never a positive authority.
    native_initial: Option<crate::collaboration::native_initial::InitialReadBridge>,
    /// How many times a record arrived in the backend without being
    /// opened here ([`RecordStore::note_kept`]).
    kept: std::cell::Cell<u64>,
    /// Records of this store's trees peers wrote that it does not hold yet
    /// ([`RecordStore::announce`]).
    announced: std::cell::RefCell<std::collections::BTreeSet<RecordId>>,
    /// Records the workspace's hosts sent everything they hold of, until something of them arrives.
    answered: std::cell::RefCell<std::collections::BTreeSet<RecordId>>,
    /// The windows that read this store's records, woken when another replica's change arrives,
    /// which no input or wake of theirs announces.
    readers: std::cell::RefCell<Vec<crate::WakeHandle>>,
    // Drop after the writers and reservations above. Temporary profiles must
    // outlive asynchronous flushes and recovery handles, not just their host.
    _profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

struct Reserved {
    generation: u64,
    kind: RecordKind,
    /// The durable owner reserved for a store-backed record; `None` for a
    /// portable reservation, whose cache has no single-writer owner.
    store: Option<Rc<app_state::DocOwner>>,
    /// The persistent actor minted for a portable reservation.
    actor: Option<automerge::ActorId>,
}

impl RecordSet {
    /// The launch-environment set: per-record documents under this package's state directory. The
    /// host's package id wins over `manifest_id`, as the workspace index does, or a package's
    /// records would split from its index; the host's workspace id scopes them.
    pub fn open(manifest_id: impl Into<String>) -> Self {
        let environment = crate::host::environment();
        let identity = &environment.identity;
        let package_id = identity
            .package_id
            .clone()
            .unwrap_or_else(|| manifest_id.into());
        let home = identity
            .workspace_id
            .clone()
            // A launcher-less process still gets an explicit, non-routable scope: it may not
            // collaborate, but it must never fall back to the historical package-global file.
            .unwrap_or_else(|| uuid::Uuid::nil().to_string());
        Self::open_in(app_state::DocStore::native_store(), package_id, home)
    }

    /// The launch-environment set over an explicit store: each record is a shared-writer document
    /// in workspace `home`, and pre-workspace package records are imported into exactly one
    /// workspace on first open.
    pub fn open_in(
        store: Rc<dyn DurableStore>,
        package_id: impl Into<String>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            package_id: package_id.into(),
            home: home.into(),
            person: None,
            retired: None,
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            documents: Some(store),
            backend: None,
            portable: None,
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: crate::collaboration::native_initial::capture_bridge(),
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
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
            person: None,
            retired: None,
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: false,
            documents: None,
            backend: None,
            portable: None,
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: None,
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
            _profile_lifetime: None,
        }
    }

    /// The workspace these records are opened in.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Name the workspace explicitly. Used by tests and by a host that opens
    /// records on behalf of a workspace other than its own launch environment.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<String>) -> Self {
        self.home = home.into();
        self
    }

    /// The record currently checked out, if any.
    pub fn active(&self) -> Option<&RecordId> {
        self.active.as_ref()
    }

    pub(crate) fn record_active_private_action(
        &mut self,
        action: app_state::PrivateAction,
    ) -> Result<(), String> {
        let id = self.active.as_ref().ok_or("no active record")?;
        let slot = self.open.get_mut(id).ok_or("active record is not open")?;
        slot.autosave
            .as_mut()
            .ok_or("record has no private durable writer")?
            .record_private_action(action)
    }

    pub(crate) fn active_private_actions(
        &self,
        doc: &AutoCommit,
    ) -> Result<Vec<app_state::PrivateAction>, String> {
        let id = self.active.as_ref().ok_or("no active record")?;
        let slot = self.open.get(id).ok_or("active record is not open")?;
        // A session-owned record may have no process-local checkpoint store; there is then nothing
        // to recover, and writing new evidence still needs a durable writer.
        match slot.autosave.as_ref() {
            Some(writer) => writer.private_actions(doc),
            None => Ok(Vec::new()),
        }
    }

    /// Every record this process has open, checked-out one included.
    pub fn open_records(&self) -> Vec<RecordCheckout> {
        let mut out: Vec<RecordCheckout> = self
            .open
            .iter()
            .map(|(id, p)| RecordCheckout::new(id.clone(), p.kind.clone()))
            .collect();
        out.compact_sort();
        out
    }

    /// The bounded, validated names currently granted by the workspace index, without implying
    /// their documents are loaded, persisted or joined on the transport.
    pub fn known_records(&self) -> Vec<RecordCheckout> {
        self.known
            .iter()
            .map(|(id, kind)| RecordCheckout::new(id.clone(), kind.clone()))
            .collect()
    }

    /// Refresh the workspace-granted names without opening any: a removed or tombstoned entry
    /// disappears from future discovery, but open data stays in memory, since revocation cannot
    /// un-share bytes already received.
    pub fn discover_workspace(&mut self, workspace: &AutoCommit) -> Vec<RecordCheckout> {
        let found = index::discoverable_references(workspace);
        self.known = found
            .iter()
            .map(|checkout| (checkout.id.clone(), checkout.kind.clone()))
            .collect();
        found
    }

    /// [`RecordOpenError::Busy`] while `id` is lent to an action frame.
    fn not_lent(&self, id: &RecordId) -> Result<(), RecordOpenError> {
        if self.lent.contains(id) {
            Err(RecordOpenError::Busy)
        } else {
            Ok(())
        }
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

    /// The cluster a record's route names: the workspace it is kept in, or
    /// the person's own cluster for a record kept in their own scope.
    fn cluster(&self) -> Result<uuid::Uuid, RecordOpenError> {
        if let Some(person) = self.person {
            return Ok(person);
        }
        uuid::Uuid::parse_str(&self.home).map_err(|_| RecordOpenError::IdentityMismatch)
    }

    /// Primary stays `record=None` on every host.
    fn wire_route(
        &self,
        id: &RecordId,
    ) -> Result<crate::collaboration::RouteSpec, RecordOpenError> {
        let cluster = self.cluster()?;
        Ok(if id.is_package_primary() {
            crate::collaboration::RouteSpec::app(&self.package_id, cluster)
        } else {
            crate::collaboration::RouteSpec::record(&self.package_id, cluster, id.as_str())
        })
    }

    /// Native caller has already read this exact route through its admitted SDK
    /// channel. Install that graph before the legacy empty-document initializer.
    pub(crate) fn checkout_with_initial(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &RecordCheckout,
        mutation: &collab_types::DocumentMutation,
    ) -> bool {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return false;
        };
        if !want.is_valid()
            || self.backend.is_some()
            || self.portable.is_some()
            || state.doc.pending_ops() != 0
        {
            return false;
        }
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return false;
        };
        if validate_identity(&incoming, want, &self.home).is_err() {
            return false;
        }
        if self.active.is_none() && !state.doc.base_heads().is_empty() {
            // Existing, finished local work is merged, never replaced. An open
            // local transaction was refused above without forcing a commit.
            if !matches!(state.merge_remote_committed(mutation), Ok(Some(_))) {
                return false;
            }
            return self.checkout(state, workspace, want);
        }
        if let Some(slot) = self.open.get_mut(&want.id) {
            let Some(doc) = slot.doc.as_mut() else {
                return false;
            };
            if doc.pending_ops() != 0 {
                return false;
            }
            if doc.merge_incoming(&mut incoming).is_err() {
                return false;
            }
        } else {
            let Ok(autosave) = self.persist_for(want, &mut incoming) else {
                return false;
            };
            if !init_scoped(
                &mut incoming,
                &want.id,
                &want.kind,
                &self.home,
                (!self.home.is_empty()).then_some(self.home.as_str()),
            ) {
                return false;
            }
            let history = crate::state::UndoLog::new(&mut incoming);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(incoming),
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: crate::bound_dispatch::RecordActionState::default(),
                },
            );
        }
        self.checkout(state, workspace, want)
    }

    /// Switch the live document to `want`: the outgoing one is parked and flushed, the incoming
    /// loaded (or created) and referenced into the workspace's index. Returns whether anything
    /// changed, so a widget can declare its record every frame.
    pub fn checkout(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &RecordCheckout,
    ) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record checkout");
            return false;
        }
        if self.backend.is_some() || self.portable.is_some() {
            return false;
        }
        if self.active.as_ref() == Some(&want.id) {
            return false;
        }

        // A document with content and no record *becomes* this record's rather than being replaced,
        // which is "save untitled as report.odt"; replacing it would be the data loss records exist
        // to prevent.
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
            return self.finish_checkout(state, workspace, want);
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

        let incoming = if let Some((mut doc, autosave)) = prepared {
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
        } else {
            self.open
                .get_mut(&want.id)
                .and_then(|slot| slot.doc.take())
                .unwrap_or_default()
        };

        state.adopt_doc(incoming);
        self.finish_checkout(state, workspace, want)
    }

    /// Stamps identity, applies declared references and indexes the record and its references into
    /// the workspace; shared by both checkout routes.
    fn finish_checkout(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &RecordCheckout,
    ) -> bool {
        // Persistent records always have an explicit workspace: failing closed keeps a
        // launcher-less process from recreating the old package-global namespace.
        if self.persistent && self.home.is_empty() {
            tracing::error!(record = %want.id, "refusing to open a persistent record without a workspace scope");
            return false;
        }
        let scope = (!self.home.is_empty()).then_some(self.home.as_str());
        if !init_scoped(&mut state.doc, &want.id, &want.kind, &self.home, scope) {
            tracing::warn!(record = %want.id, workspace = %self.home, "record identity or workspace scope did not match stored document");
            return false;
        }
        for (target, relation) in &want.references {
            add_reference(
                &mut state.doc,
                &RecordRef::new(target.id.clone(), relation.clone()),
            );
        }
        if let Some(workspace) = workspace {
            index::reference(workspace, &want.id, &want.kind);
            // Targets too: a workspace holding the annotations but not their
            // target would hand a member a reference they can't reach.
            for (target, _) in &want.references {
                index::reference(workspace, &target.id, &target.kind);
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
    ) -> Result<(AutoCommit, Option<app_state::DocWriter>), RecordOpenError> {
        if self.persistent && self.home.is_empty() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let initial = self
            .native_initial
            .as_ref()
            .map(|bridge| {
                bridge
                    .record(&self.package_id, &self.home, want)
                    .inspect_err(|error| {
                        // A record on its way fails nothing.
                        if let crate::collaboration::native_initial::RecordReadError::Initialization(error) = error {
                            crate::collaboration::native_initial::fail(error);
                        }
                    })
            })
            .transpose()?
            .flatten();
        let mut doc = if let Some(read) = &initial {
            read.with_mutation(|mutation| {
                let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                    return Err("native initial record snapshot missing".to_string());
                };
                let doc = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
                validate_identity(&doc, want, &self.home).map_err(|error| error.to_string())?;
                Ok(doc)
            })
            .and_then(|result| result)
            .map_err(RecordOpenError::Initialization)?
        } else {
            AutoCommit::new()
        };
        let autosave = self.persist_for(want, &mut doc)?;
        if self.persistent
            && !init_scoped(&mut doc, &want.id, &want.kind, &self.home, Some(&self.home))
        {
            return Err(RecordOpenError::IdentityMismatch);
        }
        if let Some(read) = initial {
            read.applied();
        }
        Ok((doc, autosave))
    }

    fn documents(&self) -> Result<&Rc<dyn DurableStore>, app_state::StoreError> {
        self.documents
            .as_ref()
            .ok_or_else(|| app_state::StoreError::Io {
                context: "record set has no document store".into(),
                source: std::io::Error::from(std::io::ErrorKind::Unsupported),
            })
    }

    /// The shared-writer document for one record in this set's workspace.
    fn scoped_document(
        &self,
        record_key: &str,
    ) -> Result<app_state::DocStore, app_state::StoreError> {
        ::collaboration::record::save::record_document(
            self.documents()?,
            &self.package_id,
            &self.home,
            record_key,
        )
    }

    /// A former identity's document in this workspace, restamped for `want`
    /// (see [`adopt_former`]). Launch-environment route.
    fn adopt_former_scoped(
        &self,
        want: &RecordCheckout,
    ) -> Result<Option<AutoCommit>, app_state::StoreError> {
        if want.formerly.is_empty() {
            return Ok(None);
        }
        let documents = Rc::clone(self.documents()?);
        adopt_former(
            &documents,
            want,
            &self.home,
            |former| StorageKey::record(&self.package_id, &self.home, former.storage_key()),
            |key| app_state::DocStore::shared(Rc::clone(&documents), key.clone())?.load_complete(),
        )
    }

    /// Attach `doc` to this record's file: merge in what is stored and hand back the writer. The
    /// writer is seeded from the **stored** heads, not the merged ones, or adopted content would
    /// read as already saved.
    fn persist_for(
        &self,
        want: &RecordCheckout,
        doc: &mut AutoCommit,
    ) -> Result<Option<app_state::DocWriter>, RecordOpenError> {
        let id = &want.id;
        if !self.persistent {
            return Ok(None);
        }
        if self.home.is_empty() {
            return Ok(None);
        }
        let record_key = id.storage_key();
        let mut store = match self.scoped_document(&record_key) {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "record store unavailable; writes refused"
                );
                return Ok(Some(app_state::DocWriter::disabled(format!(
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
                return Ok(Some(app_state::DocWriter::disabled(format!(
                    "stored record {id} could not be read: {e}"
                ))));
            }
        };
        // A former identity's record is a read-only source, left on disk until an explicit cleanup,
        // so a crash before the first scoped autosave cannot lose it.
        let mut adopted_elsewhere = false;
        let stored = match stored {
            Some(stored) => Some(stored),
            None => match self.adopt_former_scoped(want) {
                Ok(adopted) => {
                    adopted_elsewhere = adopted.is_some();
                    adopted
                }
                Err(e) => {
                    tracing::error!(record = %id, error = %e, "former record could not be read; writes refused");
                    return Ok(Some(app_state::DocWriter::disabled(format!(
                        "former record of {id} could not be read: {e}"
                    ))));
                }
            },
        };
        // Merge transactionally. Automerge may have applied part of a failed
        // merge; never publish or enqueue that partial result to the writer.
        let mut merged = doc.clone();
        let mut stored = stored;
        if let Some(stored) = stored.as_mut()
            && let Err(error) = merged.merge_incoming(stored)
        {
            tracing::error!(record = %id, %error, "record merge refused; original stored bytes retained");
            return Ok(Some(app_state::DocWriter::disabled(format!(
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
        // The package actor belongs to its index, and a bootstrap fork actor to pre-admission
        // edits; every later canonical operation belongs to this record's original persistent
        // writer.
        merged.set_actor(store.actor().clone());
        *doc = merged;
        let mut autosave = store.writer();
        if let Some(mut stored) = stored {
            // Seed only heads already in this scoped store: an adopted document's heads are absent
            // from the new file, and seeding them would strand their dependencies after a crash.
            if !adopted_elsewhere {
                autosave.seed(&mut stored);
            }
        }
        Ok(Some(autosave))
    }

    // A host showing several records at once: none can be checked out, since all are on screen in
    // one frame, so these reach an open record where it lies, sharing the same opening, persistence
    // and history.

    /// Open `want` if it is not open already, loading its document from its own file and bringing
    /// it into the workspace's index; returns whether it opened it.
    pub fn open_record(
        &mut self,
        want: &RecordCheckout,
        workspace: Option<&mut AutoCommit>,
    ) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record open");
            return false;
        }
        if self.backend.is_some() || self.portable.is_some() {
            return matches!(self.try_open_record(want), Ok(RecordOpenOutcome::Loaded));
        }
        let fresh = !self.open.contains_key(&want.id);
        if fresh {
            let Ok((mut doc, autosave)) = self.load(want) else {
                return false;
            };
            if self.persistent && self.home.is_empty() {
                tracing::error!(record = %want.id, "refusing to open a persistent record without a workspace scope");
                return false;
            }
            let scope = (!self.home.is_empty()).then_some(self.home.as_str());
            if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, scope) {
                tracing::warn!(record = %want.id, workspace = %self.home, "record identity or workspace scope did not match stored document");
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
        if let Some(workspace) = workspace {
            index::reference(workspace, &want.id, &want.kind);
            for (target, _) in &want.references {
                index::reference(workspace, &target.id, &target.kind);
            }
        }
        self.remember(want);
        fresh
    }

    fn canonical_key(
        &self,
        want: &RecordCheckout,
    ) -> Result<(Rc<dyn DurableStore>, StorageKey), RecordOpenError> {
        let backend = self
            .backend
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        if want.id.is_package_primary() {
            // The package-primary document is the workspace's own document
            // (`<package>/rooms/<workspace>`), never a minted record beneath it.
            app_state::validate_record_identity(&self.package_id, &self.home, "index")?;
            return Ok((
                Rc::clone(backend),
                StorageKey::workspace_index(&self.package_id, &self.home),
            ));
        }
        let record = want.id.storage_key();
        app_state::validate_record_identity(&self.package_id, &self.home, &record)?;
        Ok((
            Rc::clone(backend),
            StorageKey::record(&self.package_id, &self.home, record),
        ))
    }

    /// Whether `id` is a record of the workspace rather than of one window: its own document and
    /// the records its people share, each written by every window and host, which join as shared
    /// writers. Any other record has one owner.
    fn shared_in_workspace(&self, id: &RecordId) -> bool {
        id.is_package_primary()
            || collab_types::workspace_record(&self.package_id, id.as_str()).is_some()
    }

    fn try_open_record(
        &mut self,
        want: &RecordCheckout,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        self.try_open_record_with_recovery(want, true)
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
        let (backend, key) = self.canonical_key(want)?;
        if self.pending.contains_key(&want.id) {
            return Err(RecordOpenError::Busy);
        }
        // A record opening off the frame is opened by taking it in, once it is decoded.
        if self.opening.contains_key(&want.id) {
            self.take_in_opening(&want.id, true)?;
        }
        if let Some(slot) = self.open.get(&want.id) {
            if slot.kind != want.kind {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let doc = slot.doc.as_ref().ok_or(RecordOpenError::Busy)?;
            if validate_identity(doc, want, &self.home).is_err() {
                let bridge = self
                    .native_initial
                    .as_ref()
                    .ok_or(RecordOpenError::IdentityMismatch)?;
                let read = bridge
                    .record(&self.package_id, &self.home, want)?
                    .ok_or(RecordOpenError::IdentityMismatch)?;
                validate_creation_capture(doc, want, &self.home, &read)?;
            }
            return Ok(RecordOpenOutcome::AlreadyOpen);
        }
        // A record no committed read answers (the app's own channel serves
        // it) opens below, as without a bridge.
        if let Some(read) = self
            .native_initial
            .as_ref()
            .map(|bridge| bridge.record(&self.package_id, &self.home, want))
            .transpose()?
            .flatten()
        {
            let store = if self.shared_in_workspace(&want.id) {
                app_state::DocStore::shared(backend, key)?
            } else {
                app_state::DocStore::exclusive(backend, key)?
            };
            let creation = read.clone();
            // The creation's graph is the snapshot's: its heads are those of the snapshot as it
            // is decoded to be validated first, never decoded again only to read them.
            let creation_heads: std::cell::RefCell<Option<Vec<automerge::ChangeHash>>> =
                std::cell::RefCell::default();
            let (mut doc, autosave) = canonical_read::finish_created(
                store,
                read,
                |doc| validate_identity(doc, want, &self.home).map_err(|error| error.to_string()),
                |doc| {
                    let mut heads = creation_heads.borrow_mut();
                    let heads = heads.get_or_insert_with(|| doc.base_heads());
                    validate_created_graph(
                        doc,
                        want,
                        &self.home,
                        creation.creation_workspace().map(|scope| scope.id.as_str()),
                        heads,
                    )
                    .map_err(|error| error.to_string())
                },
            )
            .map_err(RecordOpenError::Initialization)?;
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
            self.answered.borrow_mut().remove(&want.id);
            return Ok(RecordOpenOutcome::Loaded);
        }
        // On a thread that may not wait on the disk, a record's presence is told by a read, not by
        // a lease a new reservation would take again: each lease is opened by the store's own
        // thread and handed over once.
        if want.formerly.is_empty()
            && backend.declined_waiting_here()
            && backend.read(&key)?.is_none()
        {
            return Err(RecordOpenError::MissingRecord);
        }
        // The workspace's own documents are held by every window and host, each in its own process,
        // and a relaunch overlaps the instance still exiting: they join as shared writers (appends
        // interleave, every load merges) instead of refusing the second opener.
        let owner = if self.shared_in_workspace(&want.id) {
            app_state::DocOwner::acquire_shared(Rc::clone(&backend), key)?
        } else {
            app_state::DocOwner::acquire(Rc::clone(&backend), key)?
        };
        let loaded = if recover_partial {
            owner.load()?
        } else {
            owner.load_complete()?
        };
        let (mut doc, autosave) = if let Some(loaded) = loaded {
            validate_identity(loaded.document(), want, &self.home)?;
            let doc = owner.accept(loaded)?;
            let owner = Rc::new(owner);
            let autosave = app_state::DocWriter::try_start(&owner)?;
            (doc, autosave)
        } else {
            // Nothing under the new id: a record kept before the person's own store existed, or a
            // former identity's, is written in full under the new key before it is shown.
            let mut doc = match self.take_from_retired_workspace(&backend, want)? {
                Some(taken) => taken,
                None => adopt_former(
                    &backend,
                    want,
                    &self.home,
                    |former| StorageKey::record(&self.package_id, &self.home, former.storage_key()),
                    |key| {
                        Ok(
                            app_state::DocOwner::acquire(Rc::clone(&backend), key.clone())?
                                .load_complete()?
                                .map(|loaded| loaded.document().clone()),
                        )
                    },
                )?
                .ok_or(RecordOpenError::MissingRecord)?,
            };
            doc.set_actor(owner.actor().clone());
            if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, Some(&self.home)) {
                return Err(RecordOpenError::IdentityMismatch);
            }
            validate_identity(&doc, want, &self.home)?;
            commit_genesis(&mut doc);
            let owner = Rc::new(owner);
            let autosave = app_state::DocWriter::start_new(&owner, &mut doc)?;
            (doc, autosave)
        };
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

    /// An open record's document, for a host showing several at once; `None` for a record not held
    /// here or the one checked out, whose live document is the toolkit's.
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

    /// Offer every open record's document to its own writer, the multi-record twin of
    /// [`Self::tick`]. # Errors A record's cache reports that it could not take the document.
    pub fn tick_open(&mut self) -> Result<(), String> {
        if self.portable.is_some() {
            return self.flush_portable(None, false);
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
            }
        }
        Ok(())
    }

    /// Write out every open record and wait for it to land, the multi-record twin of
    /// [`Self::flush`]. # Errors Each record's writer that reports a storage failure, by record.
    pub fn flush_open(&mut self) -> Result<(), String> {
        if self.portable.is_some() {
            return self.flush_portable(None, false);
        }
        let mut errors = Vec::new();
        for (id, slot) in &mut self.open {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
                if let Err(error) = saver.try_flush() {
                    errors.push(format!("{id}: {error}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
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

    /// Flush active and parked records, reporting unavailable or failed writers. # Errors Returns
    /// record-specific persistence failures without discarding live bytes.
    pub fn try_flush(&mut self, live: &mut AutoCommit) -> Result<(), String> {
        self.try_flush_documents(Some(live), Durability::Durable)
    }

    /// [`Self::try_flush`] for a caller that must know the records are *written*, not that the
    /// store committed them: a store that commits on a thread of its own always answers "not yet".
    fn try_flush_written(&mut self) -> Result<(), String> {
        self.try_flush_documents(None, Durability::Written)
    }

    fn try_flush_documents(
        &mut self,
        mut live: Option<&mut AutoCommit>,
        wanted: Durability,
    ) -> Result<(), String> {
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
            slot.action_state.commit_for_save(doc);
            saver.save(doc);
            let flushed = match wanted {
                Durability::Durable => saver.try_flush(),
                Durability::Written => saver.try_flush_written(),
            };
            if let Err(error) = flushed {
                errors.push(format!("{id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Write out everything still pending, the live document included, and wait for it to land. #
    /// Errors Each record's writer that reports a storage failure, by record.
    pub fn flush(&mut self, live: &mut AutoCommit) -> Result<(), String> {
        self.try_flush(live)
    }
}

/// How far a flush must go: to the store's commit, or to the bytes being
/// written, where the store commits them on a thread of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Durability {
    Durable,
    Written,
}

/// A [`RecordSet`] several widgets share via a handle, so two tabs on the same file stay one
/// document; single-threaded on purpose, since a `Mutex` here would guard against nothing.
#[derive(Clone)]
pub struct RecordStore(std::rc::Rc<std::cell::RefCell<RecordSet>>);

impl RecordStore {
    /// Initialize only the exact host-created original record, never an empty restore.
    pub(crate) fn initialize_admitted_record(
        &self,
        checkout: &RecordCheckout,
        initialization: &crate::collaboration::native_initial::RecordInitialization,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        let original = self.read_checked(&checkout.id, Clone::clone)?;
        let workspace = initialization.workspace().ok_or_else(|| {
            RecordOpenError::Initialization("original creation scope unavailable".into())
        })?;
        if workspace.id != self.0.borrow().home {
            return Err(RecordOpenError::IdentityMismatch);
        }
        initialization.validate_original(checkout, |read| {
            validate_creation_capture(&original, checkout, &workspace.id, read)
        })?;
        let original_heads = original.base_heads();
        let mut document = original.clone();
        genesis(&mut document, checkout, &workspace.id, initialize)?;
        let mut preserved = original;
        document
            .merge_incoming(&mut preserved)
            .map_err(|error| RecordOpenError::Initialization(error.to_string()))?;
        seed(&mut document).map_err(RecordOpenError::Initialization)?;
        commit_genesis(&mut document);
        validate_identity(&document, checkout, &workspace.id)?;
        let bytes = document.save();
        initialization
            .commit(checkout.kind.as_str(), bytes)
            .map_err(RecordOpenError::Initialization)?;
        // Ordinary peer merges require an initialized local identity; only this exact original
        // creation may install its acknowledged genesis, and only after the blocking owner ACK,
        // never replacing intervening edits.
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if set.home != workspace.id {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let slot = set
            .open
            .get_mut(&checkout.id)
            .filter(|slot| slot.kind == checkout.kind)
            .ok_or(RecordOpenError::IdentityMismatch)?;
        let current = slot.doc.as_mut().ok_or(RecordOpenError::Busy)?;
        if current.pending_ops() != 0 || current.base_heads() != original_heads {
            return Err(RecordOpenError::IdentityMismatch);
        }
        slot.history.rebase(&mut document);
        slot.action_state.generation = slot
            .action_state
            .generation
            .and_then(|generation| generation.checked_add(1));
        *current = document;
        if let Some(writer) = &mut slot.autosave {
            writer.request(current);
        }
        Ok(())
    }
}

type OwnerIndex =
    Rc<std::cell::RefCell<HashMap<(String, String), std::rc::Weak<std::cell::RefCell<RecordSet>>>>>;

/// Explicit profile-scoped owner registry. Clones share the original registry;
/// weak entries never keep a closed writer alive.
#[derive(Clone)]
pub struct RecordOwners {
    store: Rc<dyn DurableStore>,
    /// A hydrated host-acknowledged browser cache; record sets use it
    /// instead of `store` when present.
    portable: Option<PortableStorage>,
    owners: OwnerIndex,
    profile_lifetime: Option<Rc<dyn std::any::Any>>,
}

impl RecordOwners {
    /// Owners persisting under a local profile directory.
    pub fn new(profile_state_root: PathBuf) -> Self {
        Self::with_store(Rc::new(app_state::FsStore::new(profile_state_root)))
    }

    /// Owners persisting through a host-provided store.
    pub fn with_store(store: Rc<dyn DurableStore>) -> Self {
        Self {
            store,
            portable: None,
            owners: Rc::default(),
            profile_lifetime: None,
        }
    }

    /// Retain a host-owned profile resource until its last writer/recovery owner
    /// is gone. Call this before opening any records.
    pub(crate) fn retain_profile(&mut self, lifetime: Rc<dyn std::any::Any>) {
        self.profile_lifetime = Some(lifetime);
    }

    /// Get the unique live owner for this package and workspace. # Errors Rejects an invalid
    /// package or workspace.
    pub fn open(
        &mut self,
        package_id: &str,
        workspace_id: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_with(package_id, workspace_id, true)
    }

    /// The owner of `package_id`'s records of the person, kept in `scope`: unlike [`Self::open`] it
    /// never joins a native component's construction, since the person's records are not the
    /// history that component was launched to read. # Errors Rejects an invalid package or scope.
    pub fn open_person(
        &mut self,
        package_id: &str,
        scope: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_scoped(package_id, scope, false, None)
    }

    /// The owner of `package_id`'s records of one person, kept in their own scope. Those carry one
    /// route per `(app, record)` in the person's cluster, so every device of theirs opens the same
    /// document, and their store carries it; one that derives no cluster keeps them on this device.
    /// # Errors Rejects an invalid package or scope.
    pub fn open_personal(
        &mut self,
        package_id: &str,
        person: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_scoped(
            package_id,
            person::PERSONAL_SCOPE,
            false,
            PersonScope::of(person),
        )
    }

    fn open_with(
        &mut self,
        package_id: &str,
        workspace_id: &str,
        joins_construction: bool,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_scoped(package_id, workspace_id, joins_construction, None)
    }

    fn open_scoped(
        &mut self,
        package_id: &str,
        workspace_id: &str,
        joins_construction: bool,
        person: Option<PersonScope>,
    ) -> Result<RecordStore, RecordOpenError> {
        // A cluster of the person's own routes their own scope only: a
        // workspace's records name the workspace.
        let person = person.filter(|_| workspace_id == person::PERSONAL_SCOPE);
        if self.portable.is_some() {
            app_state::validate_record_namespace(package_id, workspace_id)?;
        } else {
            app_state::validate_record_identity(package_id, workspace_id, "validation")?;
        }
        if let Some(cache) = &self.portable
            && let Some(owner) =
                cache.record_owner::<std::cell::RefCell<RecordSet>>(package_id, workspace_id)
        {
            return Ok(RecordStore(owner));
        }
        let key = (package_id.to_owned(), workspace_id.to_owned());
        if let Some(owner) = self
            .owners
            .borrow()
            .get(&key)
            .and_then(std::rc::Weak::upgrade)
        {
            return Ok(RecordStore(owner));
        }
        let store = RecordStore::new(RecordSet {
            package_id: package_id.to_owned(),
            home: workspace_id.to_owned(),
            person: person.as_ref().map(|person| person.cluster),
            retired: person.map(|person| person.retired),
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            documents: None,
            backend: self.portable.is_none().then(|| Rc::clone(&self.store)),
            portable: self.portable.clone(),
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: (joins_construction && self.portable.is_none())
                .then(crate::collaboration::native_initial::capture_bridge)
                .flatten(),
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
            _profile_lifetime: self.profile_lifetime.clone(),
        });
        if let Some(cache) = &self.portable {
            cache.register_record_owner(package_id, workspace_id, &store.0);
        }
        let entry = Rc::downgrade(&store.0);
        if let Some(index) = Rc::get_mut(&mut self.owners) {
            index.get_mut().insert(key, entry);
        } else {
            self.owners.borrow_mut().insert(key, entry);
        }
        Ok(store)
    }
}

/// The heads of what `doc` keeps without the changes `refused` names and everything built on them,
/// and a [`RecordRefusal`] saying `reason` per action the withdrawn changes carried, in order.
fn withdrawal(
    doc: &mut AutoCommit,
    refused: &[automerge::ChangeHash],
    reason: &str,
) -> (Vec<automerge::ChangeHash>, Vec<RecordRefusal>) {
    let changes = doc.get_changes_meta(&[]);
    let mut withdrawn: std::collections::HashSet<automerge::ChangeHash> =
        std::collections::HashSet::new();
    let mut refusals = Vec::new();
    // The history lists every change after its dependencies.
    for change in &changes {
        if !refused.contains(&change.hash) && !change.deps.iter().any(|dep| withdrawn.contains(dep))
        {
            continue;
        }
        withdrawn.insert(change.hash);
        let claimed = collab_types::ChangeClaim::of_change(
            change.hash.0,
            change.message.as_deref(),
            Vec::new(),
        );
        if let Ok(claim) = claimed {
            for (action, item) in claim.action.named() {
                let refusal = RecordRefusal {
                    action,
                    item,
                    reason: reason.to_owned(),
                };
                if !refusals.contains(&refusal) {
                    refusals.push(refusal);
                }
            }
        }
    }
    let depended: std::collections::HashSet<automerge::ChangeHash> = changes
        .iter()
        .filter(|change| !withdrawn.contains(&change.hash))
        .flat_map(|change| change.deps.iter().copied())
        .collect();
    let kept = changes
        .iter()
        .map(|change| change.hash)
        .filter(|hash| !withdrawn.contains(hash) && !depended.contains(hash))
        .collect();
    (kept, refusals)
}

/// An action whose change the workspace refused: the record withdrew it, and the app holding the
/// record is told, as it is of an action refused on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRefusal {
    /// The verb's key, or the edited field's path
    /// ([`collab_types::ClaimedAction::named`]).
    pub action: String,
    /// The item it acted on, if any.
    pub item: Option<String>,
    /// Why, for a person.
    pub reason: String,
}

/// What windows said they did to a record, held until the workspace it is shared in keeps the
/// change, so success is never said of a change the workspace then refuses. An unreplicated record
/// is decided by nobody else, so it is said at once; a refusal withdraws the changes and drops the
/// notice.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceNotices {
    /// A host replicates the record to a host that decides its changes.
    replicated: bool,
    held: Vec<HeldNotice>,
    /// The heads the last publication carried.
    sent: Vec<automerge::ChangeHash>,
    /// The workspace answered every commit published so far.
    answered: bool,
    said: Vec<String>,
}

#[derive(Debug)]
struct HeldNotice {
    text: String,
    heads: Vec<automerge::ChangeHash>,
    /// A publication carried its heads.
    published: bool,
}

impl WorkspaceNotices {
    fn hold(&mut self, text: String, heads: Vec<automerge::ChangeHash>) {
        let unchanged = heads == self.sent;
        if !self.replicated || (unchanged && self.answered) {
            self.said.push(text);
            return;
        }
        self.held.push(HeldNotice {
            text,
            heads,
            published: unchanged,
        });
    }

    /// A host replicating the record asked for what to publish.
    fn replicating(&mut self) {
        self.replicated = true;
    }

    /// A publication carried the record up to `heads`.
    fn sent(&mut self, heads: &[automerge::ChangeHash]) {
        self.replicated = true;
        heads.clone_into(&mut self.sent);
        self.answered = false;
        for notice in &mut self.held {
            notice.published = true;
        }
    }

    /// The workspace's host answered every commit published so far.
    fn settled(&mut self) {
        self.answered = true;
        let (published, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|notice| notice.published);
        self.held = waiting;
        self.said
            .extend(published.into_iter().map(|notice| notice.text));
    }

    /// Changes were withdrawn: what was said at heads `doc` no longer holds
    /// is not said.
    fn withdrawn(&mut self, doc: &mut AutoCommit) {
        self.held.retain(|notice| {
            notice
                .heads
                .iter()
                .all(|head| doc.get_change_meta_by_hash(head).is_some())
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordOpenOutcome {
    Loaded,
    AlreadyOpen,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordOpenError {
    #[error(transparent)]
    Store(app_state::StoreError),
    #[error("record identity does not match its binding")]
    IdentityMismatch,
    #[error("record does not exist")]
    MissingRecord,
    #[error("record is reserved or already initialized")]
    Busy,
    /// The store's own thread is still opening the record's document for a thread that must not
    /// wait on storage; that thread's wake is called when it is done.
    #[error("the document is still opening")]
    Opening,
    #[error("the new record identity already exists or is reserved")]
    AlreadyExists,
    #[error("record initialization failed: {0}")]
    Initialization(String),
    /// A record the workspace sends only once asked is not here yet: it was asked for, and the
    /// store wakes its readers when it arrives or when the workspace's hosts say they have none of
    /// it.
    #[error("the record is on its way from the workspace")]
    Arriving,
    /// The record left this workspace (see [`transfer`]); its history stays, but
    /// it is never opened as a live document again.
    #[error("record was moved out of this workspace")]
    Retired,
}

impl RecordOpenError {
    /// Whether another process was writing the store at this moment: the record is not one this
    /// device cannot open, just one it did not open this time, and opening it again once that write
    /// is done succeeds.
    #[must_use]
    pub fn writer_busy(&self) -> bool {
        matches!(self, Self::Store(app_state::StoreError::WriterBusy { .. }))
    }
}

impl From<app_state::StoreError> for RecordOpenError {
    fn from(error: app_state::StoreError) -> Self {
        match error {
            app_state::StoreError::Opening(_) => Self::Opening,
            other => Self::Store(other),
        }
    }
}

impl From<durable_store::StoreError> for RecordOpenError {
    fn from(error: durable_store::StoreError) -> Self {
        Self::from(app_state::StoreError::from(error))
    }
}

/// The stable namespace string of a portable cache, shared by record sets and
/// instance views so both bind to the same explicit profile/device identity.
pub(crate) fn portable_namespace(cache: &PortableStorage) -> String {
    match cache.namespace() {
        crate::portable_storage::ProfileNamespace::Portable { profile, device } => {
            format!("portable:{profile}:{device}")
        }
        crate::portable_storage::ProfileNamespace::Native(root) => root.display().to_string(),
    }
}

fn primary_document(doc: &AutoCommit) -> bool {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => true,
        ObjectAt::Object(object) => {
            matches!(string_at(doc, &object, "id"), StringAt::Missing)
                && matches!(string_at(doc, &object, "kind"), StringAt::Missing)
                && !matches!(string_at(doc, &object, "home"), StringAt::Invalid)
                && !matches!(stored_workspace(doc, &object), StringAt::Invalid)
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
    match object_at(doc, &ROOT, META, ObjType::Map) {
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
    if transfer::is_retired(doc) {
        return Err(RecordOpenError::Retired);
    }
    if want.id.is_package_primary() {
        return if primary_document(doc)
            && !want.kind.as_str().is_empty()
            && !workspace.is_empty()
            && workspace_scope(doc).is_none_or(|scope| scope == workspace)
        {
            Ok(())
        } else {
            Err(RecordOpenError::IdentityMismatch)
        };
    }
    if identity(doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
        || want.kind.as_str().is_empty()
        || home(doc).is_none_or(|home| home.is_empty())
        || workspace_scope(doc).as_deref() != Some(workspace)
    {
        return Err(RecordOpenError::IdentityMismatch);
    }
    Ok(())
}

fn validate_created_identity(
    doc: &AutoCommit,
    want: &RecordCheckout,
    workspace: &str,
    creation_workspace: Option<&str>,
) -> Result<(), RecordOpenError> {
    if creation_workspace != Some(workspace)
        || workspace.is_empty()
        || !want.is_valid_local_storage_request()
        || !primary_document(doc)
        || home(doc).is_some_and(|found| found != workspace)
        || workspace_scope(doc).is_some_and(|found| found != workspace)
        || transfer::is_retired(doc)
    {
        return Err(RecordOpenError::IdentityMismatch);
    }
    Ok(())
}

impl From<crate::collaboration::native_initial::RecordReadError> for RecordOpenError {
    fn from(error: crate::collaboration::native_initial::RecordReadError) -> Self {
        use crate::collaboration::native_initial::RecordReadError;
        match error {
            RecordReadError::IdentityMismatch => Self::IdentityMismatch,
            RecordReadError::Arriving => Self::Arriving,
            RecordReadError::Initialization(error) => Self::Initialization(error),
        }
    }
}

pub(crate) fn validate_creation_capture(
    doc: &AutoCommit,
    want: &RecordCheckout,
    workspace: &str,
    read: &crate::collaboration::CommittedDocument,
) -> Result<(), RecordOpenError> {
    let heads = creation_capture_heads(read)?;
    validate_created_graph(
        doc,
        want,
        workspace,
        read.creation_workspace().map(|scope| scope.id.as_str()),
        &heads,
    )
}

fn validate_created_graph(
    doc: &AutoCommit,
    want: &RecordCheckout,
    workspace: &str,
    creation_workspace: Option<&str>,
    heads: &[automerge::ChangeHash],
) -> Result<(), RecordOpenError> {
    validate_created_identity(doc, want, workspace, creation_workspace)?;
    if doc.pending_ops() != 0 || doc.base_heads() != heads {
        return Err(RecordOpenError::IdentityMismatch);
    }
    Ok(())
}

fn creation_capture_heads(
    read: &crate::collaboration::CommittedDocument,
) -> Result<Vec<automerge::ChangeHash>, RecordOpenError> {
    read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err(RecordOpenError::Initialization(
                "creation capture is not a snapshot".into(),
            ));
        };
        let original = AutoCommit::load(bytes).map_err(|_| {
            RecordOpenError::Initialization("creation capture snapshot cannot be parsed".into())
        })?;
        Ok(original.base_heads())
    })
    .map_err(RecordOpenError::Initialization)?
}

#[derive(Clone)]
pub(crate) struct RecordAdmission {
    checkout: RecordCheckout,
    workspace: String,
}

impl RecordAdmission {
    pub(crate) fn matches(&self, doc: &AutoCommit, checkout: &RecordCheckout) -> bool {
        if &self.checkout != checkout {
            return false;
        }
        if checkout.id.is_package_primary() {
            // Primary documents carry no minted identity; see `admission`.
            return validate_identity(doc, checkout, &self.workspace).is_ok();
        }
        identity(doc).as_ref() == Some(checkout)
    }
}

/// The address the chain `chain` (whose parts are [`RecordId::part`]) is
/// published under, as one resource beside its app's own.
#[must_use]
pub fn chain_resource_id(chain: &RecordId) -> String {
    format!("chain:{}", chain.as_str())
}

/// The address the child records of `kind` that `parent` lists are
/// published under, as one container resource beside the app's own.
#[must_use]
pub fn children_resource_id(parent: &RecordId, kind: &str) -> String {
    format!("children:{kind}@{}", parent.as_str())
}

/// The chain a resource published under `id` is ([`chain_resource_id`]),
/// or `None` when `id` names no chain.
#[must_use]
pub fn chain_of_resource_id(id: &str) -> Option<RecordId> {
    RecordId::parse(id.strip_prefix("chain:")?)
}

/// The actor [`genesis`] makes record `id` of `workspace` under.
fn genesis_actor_of(id: &RecordId, kind: &RecordKind, workspace: &str) -> automerge::ActorId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new()
        .chain_update(b"tilo.record.genesis\0")
        .chain_update(id.as_str().as_bytes())
        .chain_update(b"\0")
        .chain_update(kind.as_str().as_bytes())
        .chain_update(b"\0")
        .chain_update(workspace.as_bytes())
        .finalize();
    automerge::ActorId::from(digest.get(..16).unwrap_or_default())
}

/// The actor `author`'s mount edits a record as, when the record's writer is `record_writer` and
/// the host asked for per-author writers; mounts of one profile and device share a document, so
/// this keeps each author's edits apart. Unique per writer, and the same after a reopen, so history
/// names the same author.
pub fn author_writer(record_writer: &automerge::ActorId, author: &str) -> automerge::ActorId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new()
        .chain_update(b"tilo.record.author-writer\0")
        .chain_update(record_writer.to_bytes())
        .chain_update(b"\0")
        .chain_update(author.as_bytes())
        .finalize();
    automerge::ActorId::from(digest.get(..16).unwrap_or_default())
}

/// The actor that made the genesis of the record `doc` holds: nobody edits as it, and its changes
/// carry no time, so history lists none of them.
pub fn genesis_actor(doc: &AutoCommit) -> Option<automerge::ActorId> {
    let checkout = identity(doc)?;
    let home = str_at(doc, &meta_ro(doc)?, "home")?;
    Some(genesis_actor_of(&checkout.id, &checkout.kind, &home))
}

/// Make `doc` the record as it is created: its identity and the app's `initialize`d content in one
/// change, byte for byte the same on every peer. An actor derived from the record's identity makes
/// it at time zero, so two members creating the same record hash the same change.
fn genesis(
    doc: &mut AutoCommit,
    checkout: &RecordCheckout,
    workspace: &str,
    initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
) -> Result<(), RecordOpenError> {
    let actor = genesis_actor_of(&checkout.id, &checkout.kind, workspace);
    let mut made = AutoCommit::new().with_actor(actor.clone());
    for key in doc.keys(&ROOT).collect::<Vec<_>>() {
        if let Ok(Some((Value::Scalar(value), _))) = doc.get(&ROOT, key.as_str()) {
            made.put(&ROOT, key.as_str(), value.into_owned())
                .map_err(|error| RecordOpenError::Initialization(error.to_string()))?;
        }
    }
    // Stamped, not committed: the identity is part of the one genesis change.
    let stamped = if checkout.id.is_package_primary() {
        init_scoped(
            &mut made,
            &checkout.id,
            &checkout.kind,
            workspace,
            Some(workspace),
        )
    } else {
        RecordId::parse(checkout.id.as_str()).is_some()
            && checkout.kind.is_valid()
            && stamp_scoped(
                &mut made,
                &checkout.id,
                &checkout.kind,
                workspace,
                Some(workspace),
            )
    };
    if !stamped {
        return Err(RecordOpenError::IdentityMismatch);
    }
    initialize(&mut made).map_err(RecordOpenError::Initialization)?;
    // The app's surface is the same wherever its package is installed: named
    // in the genesis, not in a change of this device's afterwards.
    let _ = stamp_surface(&mut made);
    // Every member must derive the same genesis, so the initializer may not
    // author it under an actor of its own.
    if made.get_actor() != &actor {
        return Err(RecordOpenError::IdentityMismatch);
    }
    made.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    let changes = made.get_changes(&[]);
    let mut created = AutoCommit::new().with_actor(doc.get_actor().clone());
    ApplyIncoming::apply_incoming(&mut created, changes)
        .map_err(|error| RecordOpenError::Initialization(error.to_string()))?;
    *doc = created;
    Ok(())
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

    /// Privately initialize once, then create the one history and writer and publish Ready. #
    /// Errors Retains the reservation on initializer, validation or worker-startup failure.
    pub fn initialize(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        self.initialize_seeded(initialize, |_| Ok(()))
    }

    /// [`Self::initialize`], then `seed` the record with what depends on who creates it or where;
    /// the seed follows the genesis and precedes undo history, so it is nobody's undo step. #
    /// Errors As [`Self::initialize`]; a failing seed fails the initialization.
    pub fn initialize_seeded(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        if self.store.0.borrow().portable.is_some() {
            return self.initialize_portable(initialize, seed);
        }
        self.initialize_with(initialize, seed, app_state::DocWriter::start_new)
    }

    fn initialize_with(
        &self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        start: impl FnOnce(
            &Rc<app_state::DocOwner>,
            &mut AutoCommit,
        ) -> Result<app_state::DocWriter, app_state::StoreError>,
    ) -> Result<(), RecordOpenError> {
        let (owner, workspace) = {
            let set = self.store.0.borrow();
            let pending = set
                .pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(RecordOpenError::Busy)?;
            (
                Rc::clone(
                    pending
                        .store
                        .as_ref()
                        .ok_or(RecordOpenError::IdentityMismatch)?,
                ),
                set.home.clone(),
            )
        };
        let mut doc = owner.new_document();
        genesis(&mut doc, &self.checkout, &workspace, initialize)?;
        seed(&mut doc).map_err(RecordOpenError::Initialization)?;
        validate_identity(&doc, &self.checkout, &workspace)?;
        commit_genesis(&mut doc);
        let mut set = self.store.0.borrow_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_none_or(|p| p.generation != self.generation)
        {
            return Err(RecordOpenError::Busy);
        }
        let autosave = start(&owner, &mut doc)?;
        let history = crate::state::UndoLog::new(&mut doc);
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

/// A record lent to an action frame ([`RecordStore::with_action_state`]),
/// returned to its set when the frame ends, however it ends.
struct RecordLoan<'a> {
    store: &'a RecordStore,
    id: &'a RecordId,
    parts: Option<(
        AutoCommit,
        crate::state::UndoLog,
        crate::bound_dispatch::RecordActionState,
    )>,
}

impl Drop for RecordLoan<'_> {
    fn drop(&mut self) {
        let Some((doc, history, action_state)) = self.parts.take() else {
            return;
        };
        let Ok(mut set) = self.store.0.try_borrow_mut() else {
            tracing::error!(record = %self.id, "a lent record could not be returned to its set");
            return;
        };
        set.lent.remove(self.id);
        if let Some(parked) = set.open.get_mut(self.id) {
            parked.doc = Some(doc);
            parked.history = history;
            parked.action_state = action_state;
        }
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
        let doc = set
            .peek(&checkout.id)
            .ok_or(RecordOpenError::MissingRecord)?;
        validate_identity(doc, checkout, &set.home)?;
        set.wire_route(&checkout.id)
    }

    /// The route native component admission joins before the factory runs: the open record's, a
    /// live New reservation's, the workspace's record while it opens off the frame, or an original
    /// creation whose genesis the factory has yet to write.
    pub(crate) fn admission_route(
        &self,
        checkout: &RecordCheckout,
    ) -> Result<crate::collaboration::RouteSpec, RecordOpenError> {
        {
            let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
            if let Some(reserved) = set.pending.get(&checkout.id) {
                if reserved.kind != checkout.kind {
                    return Err(RecordOpenError::IdentityMismatch);
                }
                return set.wire_route(&checkout.id);
            }
            if let Some(route) = set.opening_route(checkout) {
                return route;
            }
            if set
                .open
                .get(&checkout.id)
                .is_some_and(|slot| slot.kind != checkout.kind)
            {
                return Err(RecordOpenError::IdentityMismatch);
            }
            if let Some(doc) = set.peek(&checkout.id)
                && validate_identity(doc, checkout, &set.home).is_err()
            {
                let bridge = set
                    .native_initial
                    .as_ref()
                    .ok_or(RecordOpenError::IdentityMismatch)?;
                let read = bridge
                    .record(&set.package_id, &set.home, checkout)?
                    .ok_or(RecordOpenError::IdentityMismatch)?;
                validate_creation_capture(doc, checkout, &set.home, &read)?;
                return set.wire_route(&checkout.id);
            }
        }
        self.route(checkout)
    }

    /// Schedule this already committed History result through its existing writer, without sweeping
    /// or closing other owners' unfinished edits. # Errors The cache could not take the document,
    /// or the store is busy and took nothing.
    pub fn schedule_committed_history(&self, id: &RecordId) -> Result<(), String> {
        let Ok(mut set) = self.0.try_borrow_mut() else {
            return Err("record store is busy".to_string());
        };
        if set.portable.is_some() {
            if set
                .open
                .get(id)
                .and_then(|slot| slot.doc.as_ref())
                .is_some_and(|doc| doc.pending_ops() == 0)
            {
                return set.flush_portable(Some(id), false);
            }
            return Ok(());
        }
        let Some(slot) = set.open.get_mut(id) else {
            return Ok(());
        };
        if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut())
            && doc.pending_ops() == 0
        {
            saver.request(doc);
        }
        Ok(())
    }

    /// Cheap content witness for retained projections over open records: base heads cover committed
    /// changes, `pending_ops` the live typing transaction, and the record identity makes opening or
    /// closing a document invalidate a snapshot.
    pub fn content_revision(&self) -> String {
        let set = self.0.borrow();
        let mut records = set.open.iter().collect::<Vec<_>>();
        records.compact_sort_by_key(|(id, _)| *id);
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

    /// Captured storage identity for History; never samples ambient launch scope.
    pub(crate) fn history_namespace(
        &self,
        id: &RecordId,
    ) -> Result<(Option<String>, String, String), RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        let storage = set
            .portable
            .as_ref()
            .map(portable_namespace)
            .or_else(|| {
                set.backend
                    .as_ref()
                    .map(|store| store.namespace().to_owned())
            })
            .or_else(|| {
                parked
                    .autosave
                    .as_ref()
                    .filter(|saver| saver.is_enabled())
                    .map(|saver| saver.document_path().to_string_lossy().into_owned())
            });
        Ok((storage, set.package_id.clone(), set.home.clone()))
    }
    /// Whether the store can be read now: no caller holds it mutably.
    pub(crate) fn readable(&self) -> bool {
        self.0.try_borrow().is_ok()
    }
    /// The package and home workspace this store's records belong to, which name their replication
    /// route, and the durable store they live in when it has one.
    pub(crate) fn backend(&self) -> Option<Rc<dyn DurableStore>> {
        let set = self.0.borrow();
        set.backend.clone().or_else(|| set.documents.clone())
    }

    pub fn package_and_home(&self) -> (String, String) {
        let set = self.0.borrow();
        (set.package_id.clone(), set.home.clone())
    }

    /// The numbers of the parts of the chain `base` this store holds, in order: those open here,
    /// and those kept on this device unopened. Listing reads the store's index, so a frame loop
    /// asks on [`Self::kept_revision`] instead of every frame.
    #[must_use]
    pub fn held_parts(&self, base: &RecordId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut held: std::collections::BTreeSet<u64> = set
            .open
            .keys()
            .chain(set.lent.iter())
            .filter_map(RecordId::chain_part)
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect();
        if !set.home.is_empty() {
            // What the person kept before their own store existed is listed with what it holds now:
            // asking for a part opens it, which takes that record into the store.
            let kept: Vec<String> = std::iter::once(set.home.as_str())
                .chain(set.retired.as_deref())
                .flat_map(|workspace| {
                    match (
                        &set.portable,
                        set.backend.as_ref().or(set.documents.as_ref()),
                    ) {
                        (Some(cache), _) => cache.records(&set.package_id, workspace),
                        (None, Some(store)) => store
                            .records(&set.package_id, workspace)
                            .unwrap_or_default(),
                        (None, None) => Vec::new(),
                    }
                })
                .collect();
            // A portable cache keys a record by its id, a store by its
            // storage key.
            held.extend(kept.iter().filter_map(|key| {
                base.part_of_storage_key(key).or_else(|| {
                    RecordId::parse(key)?
                        .chain_part()
                        .filter(|(chain, _)| chain == base)
                        .map(|(_, index)| index)
                })
            }));
        }
        held.into_iter().collect()
    }

    /// Tell the store that a record it does not have open was kept in its backend by someone else
    /// (a workspace's replication, delivering a part another device started), so what follows the
    /// records it keeps looks again. Callable while a frame reads the store.
    pub fn note_kept(&self) {
        if let Ok(set) = self.0.try_borrow() {
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// Peers wrote `records` of this store's trees, which it does not hold: whatever follows those
    /// trees takes them up at once, as after [`Self::note_kept`]. Callable while a frame reads the
    /// store.
    pub fn announce(&self, records: impl IntoIterator<Item = RecordId>) {
        if let Ok(set) = self.0.try_borrow() {
            set.announced
                .borrow_mut()
                .extend(records.into_iter().filter(|record| record.tree().is_some()));
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// The numbers of the parts of the chain `base` peers wrote
    /// ([`Self::announce`]), in order.
    #[must_use]
    pub fn announced_parts(&self, base: &RecordId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let announced = set.announced.borrow();
        announced
            .iter()
            .filter_map(RecordId::chain_part)
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect()
    }

    /// Moves whenever the store is told something arrived for what follows
    /// it: [`Self::note_kept`], [`Self::announce`], [`Self::note_workspace_answered`].
    #[must_use]
    pub fn kept_revision(&self) -> u64 {
        self.0.try_borrow().map_or(0, |set| set.kept.get())
    }

    /// The numbers of the parts of the chain `base` open here and ready to read, in order. Reads no
    /// index, so it is cheap enough for every frame.
    #[must_use]
    pub fn open_parts(&self, base: &RecordId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut open: Vec<u64> = set
            .open
            .iter()
            .filter(|(_, parked)| parked.doc.is_some())
            .filter_map(|(id, _)| id.chain_part())
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect();
        open.compact_sort();
        open
    }

    /// Every window default record of `kind` this package keeps in this workspace, sorted by id, as
    /// last made durable: what an app's windows started, so a closed window's document is reached
    /// again. Read without taking a writer; a record that cannot be read, is retired or is not of
    /// `kind` here is left out and logged.
    pub fn window_records(&self, kind: &RecordKind) -> Vec<(RecordId, AutoCommit)> {
        let Some(store) = self.backend() else {
            return Vec::new();
        };
        let (package, home) = self.package_and_home();
        if home.is_empty() {
            return Vec::new();
        }
        let keys = match store.records(&package, &home) {
            Ok(keys) => keys,
            Err(error) => {
                tracing::warn!(%package, workspace = %home, %error, "listing the workspace's records failed");
                return Vec::new();
            }
        };
        // Windows of the person's kept before their own store existed are listed with the ones it
        // starts here; opening one takes it into the store.
        let ids = keys
            .iter()
            .filter_map(|key| RecordId::window_default_of_storage_key(key))
            .collect::<Vec<_>>();
        let mut found = ids
            .iter()
            .filter_map(|id| {
                let origin = transfer::Origin::Record(RecordCheckout::new(id.clone(), kind.clone()));
                match transfer::read(&store, &package, &home, &origin) {
                    Ok(doc) => Some((id.clone(), doc)),
                    Err(error) => {
                        tracing::warn!(record = %id, workspace = %home, %error, "window record not readable; left out");
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        found.extend(self.0.borrow().retired_window_records(&store, kind, &ids));
        found.compact_sort_by(|a, b| a.0.cmp(&b.0));
        found
    }
    /// Explicit immutable identity only: the store namespace, package and
    /// workspace. Legacy and memory sets have no canonical namespace.
    pub(crate) fn canonical_namespace(&self) -> Option<(String, String, String)> {
        let set = self.0.borrow();
        Some((
            match (&set.portable, &set.backend) {
                (Some(cache), _) => portable_namespace(cache),
                (None, backend) => backend.as_ref()?.namespace().to_owned(),
            },
            set.package_id.clone(),
            set.home.clone(),
        ))
    }

    /// Open Existing only; a missing document is never implicitly initialized. # Errors Reports
    /// ownership contention, absence, identity or schema refusal and storage failures.
    pub fn try_open_record(
        &self,
        want: &RecordCheckout,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        self.0
            .try_borrow_mut()
            .map_err(|_| RecordOpenError::Busy)?
            .try_open_record(want)
    }

    /// Reserve a New identity with its gate and no document/history/writer yet:
    /// the workspace's own records are reserved as shared writers, any other as
    /// its only writer.
    ///
    /// # Errors
    /// Refuses collisions, malformed existing data and unavailable storage.
    pub fn reserve_record(
        &self,
        want: &RecordCheckout,
    ) -> Result<RecordReservation, RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if !want.is_valid_local_storage_request() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        if set.portable.is_some() {
            return self.reserve_portable(&mut set, want);
        }
        // A record the app's own channel answers (one a sealed workspace does not
        // hold yet) is new, and the app makes it: reserved as without a
        // bridge. Every other new record waits for its creation admission.
        if set
            .native_initial
            .as_ref()
            .is_some_and(|bridge| !bridge.answered_by_own_channel(want))
        {
            return Err(RecordOpenError::Initialization(
                "native new document requires governor creation admission before canonical reservation".into(),
            ));
        }
        let (backend, key) = set.canonical_key(want)?;
        if set.pending.contains_key(&want.id) || set.open.contains_key(&want.id) {
            return Err(RecordOpenError::Busy);
        }
        // The reservation becomes the live writer, so the workspace's own documents
        // are joined as shared writers here too; see `try_open_record`.
        let owner = if set.shared_in_workspace(&want.id) {
            app_state::DocOwner::acquire_shared(backend, key)?
        } else {
            app_state::DocOwner::acquire(backend, key)?
        };
        if owner.load()?.is_some() {
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
                kind: want.kind.clone(),
                store: Some(Rc::new(owner)),
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
        self.0.try_borrow().is_ok_and(|set| set.peek(id).is_some())
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
            return Ok(RecordSaveStatus::of_writer(doc, saver));
        }
        Ok(if set.persistent {
            RecordSaveStatus::Failed("record persistence unavailable".to_owned())
        } else {
            RecordSaveStatus::MemoryOnly
        })
    }

    /// Have `wake` called whenever [`Self::save_status`] of `id` may change
    /// without input: its store resolving its writer's writes, or a host
    /// acknowledging a portable cache. A memory-only record never changes.
    /// Sample again once armed: a write that landed before is not heard of.
    pub fn wake_on_save_progress(&self, id: &RecordId, wake: durable_store::ReadyWake) {
        let Ok(set) = self.0.try_borrow() else {
            return;
        };
        if let Some(cache) = &set.portable {
            cache.watch_durability(&portable::key(&set.package_id, &set.home, id), wake);
            return;
        }
        if let Some(saver) = set.open.get(id).and_then(|slot| slot.autosave.as_ref()) {
            saver.wake_on_settle(wake);
        }
    }

    /// Block until the store has committed or refused every write `id`'s
    /// writer already submitted, without submitting its pending edits: the
    /// acknowledgement [`Self::save_status`] reports, awaited instead of
    /// polled.
    ///
    /// # Errors
    /// Reports missing/busy records, an unpersisted record, and the writer's
    /// outstanding failure.
    pub fn await_submitted(&self, id: &RecordId) -> Result<(), String> {
        let set = self
            .0
            .try_borrow()
            .map_err(|_| "record store is busy".to_string())?;
        let slot = set
            .open
            .get(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let saver = slot
            .autosave
            .as_ref()
            .ok_or_else(|| format!("{id}: record is not persisted"))?;
        saver.try_flush().map_err(|error| format!("{id}: {error}"))
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
            .and_then(app_state::DocWriter::last_error)
    }

    /// Stage private evidence on the exact record owner, never its CRDT.
    pub(crate) fn record_private_action(
        &self,
        id: &RecordId,
        action: app_state::PrivateAction,
    ) -> Result<(), String> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| "record store is busy")?;
        let slot = set.open.get_mut(id).ok_or("record is not open")?;
        slot.autosave
            .as_mut()
            .ok_or("record has no private durable writer")?
            .record_private_action(action)
    }

    /// Read bounded evidence against this owner's live history, without cloning
    /// the document or opening a second owner.
    pub(crate) fn private_actions(
        &self,
        id: &RecordId,
    ) -> Result<Vec<app_state::PrivateAction>, String> {
        let set = self.0.try_borrow().map_err(|_| "record store is busy")?;
        let slot = set.open.get(id).ok_or("record is not open")?;
        let doc = slot.doc.as_ref().ok_or("record document is lent")?;
        // Absence of a local checkpoint store is not failed recovery. In
        // particular, session-owned records need not have a local writer.
        // record_private_action continues to refuse writes without one.
        match slot.autosave.as_ref() {
            Some(writer) => writer.private_actions(doc),
            None => Ok(Vec::new()),
        }
    }

    /// Offer one serviced canonical record without awaiting host ACK or
    /// touching another record. Accepted bytes remain Saving until durable.
    pub(crate) fn offer_record(&self, id: &RecordId) -> Result<(), String> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| "record store is busy".to_owned())?;
        if set.portable.is_some() {
            return set.offer_portable_record(id);
        }
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
        slot.action_state.commit_for_save(doc);
        saver.offer(doc)
    }

    /// Close/retry one actual owner without settling another record's edit.
    pub(crate) fn try_flush_record(&self, id: &RecordId) -> Result<(), String> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| "record store is busy".to_string())?;
        if set.portable.is_some() {
            return set.flush_portable(Some(id), false);
        }
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
        slot.action_state.commit_for_save(doc);
        saver.save(doc);
        saver.try_flush().map_err(|error| format!("{id}: {error}"))
    }

    /// Request current changes once and await actual acknowledgements for every record.
    ///
    /// # Errors
    /// Returns record-specific storage failures; a subsequent explicit retry sends a full checkpoint.
    pub fn try_flush(&self) -> Result<(), String> {
        let mut set = self.0.borrow_mut();
        if set.portable.is_some() {
            return set.flush_portable(None, false);
        }
        set.try_flush_documents(None, Durability::Durable)
    }

    /// Write out what is not stored yet, and report only what failed: the
    /// records are the store's from then on, and the store's own thread
    /// commits them ([`RecordSet::try_flush_written`]).
    pub fn try_flush_written(&self) -> Result<(), String> {
        let mut set = self.0.borrow_mut();
        if set.portable.is_some() {
            return set.flush_portable(None, true);
        }
        set.try_flush_written()
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
            set.backend.is_some() || set.portable.is_some()
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
            let key = want.id.storage_key();
            let mut owner = set.scoped_document(&key)?;
            // An empty/corrupt document is existing evidence too, never
            // permission to replace it with a freshly initialized one.
            if owner.load_complete()?.is_some() || owner.has_stored()? {
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
        commit_genesis(&mut doc);
        let mut autosave = owner.map(|owner| owner.writer());
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

    /// Existing-only admission for presentation owners. Loads a saved graph
    /// without seeding missing content. Package-global records retain the
    /// existing explicit one-workspace migration claim and their original history.
    pub(crate) fn open_existing_record_checked(
        &self,
        want: &RecordCheckout,
    ) -> Result<(), RecordOpenError> {
        let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
        if set.backend.is_some() || set.portable.is_some() {
            return set.try_open_record_with_recovery(want, false).map(|_| ());
        }
        if !set.open.contains_key(&want.id) && set.native_initial.is_some() {
            let (mut doc, mut autosave) = set.load(want)?;
            validate_identity(&doc, want, &set.home)?;
            if let Some(saver) = autosave.as_mut() {
                saver.request(&mut doc);
            }
            let history = crate::state::UndoLog::new(&mut doc);
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
        }
        if !set.open.contains_key(&want.id) {
            if !set.persistent {
                return Err(RecordOpenError::MissingRecord);
            }
            if set.home.is_empty() {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let key = want.id.storage_key();
            let mut owner = set.scoped_document(&key)?;
            let (mut doc, adopted) = if let Some(doc) = owner.load_complete()? {
                (doc, false)
            } else {
                if owner.has_stored()? {
                    return Err(app_state::StoreError::EmptyExistingDocument {
                        path: owner.document_path().to_path_buf(),
                    }
                    .into());
                }
                let Some(mut doc) = set.adopt_former_scoped(want)? else {
                    return Err(RecordOpenError::MissingRecord);
                };
                if !init_scoped(&mut doc, &want.id, &want.kind, &set.home, Some(&set.home)) {
                    return Err(RecordOpenError::IdentityMismatch);
                }
                (doc, true)
            };
            validate_identity(&doc, want, &set.home)?;
            if home(&doc).as_deref() != Some(set.home.as_str()) {
                return Err(RecordOpenError::IdentityMismatch);
            }
            let mut autosave = owner.writer();
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
            || workspace_scope(doc).as_deref() != expected_scope
        {
            return Err(RecordOpenError::IdentityMismatch);
        }
        Ok(())
    }

    /// Exact runtime owner identity; namespace equality is insufficient for reentry.
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
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
        if checkout.id.is_package_primary() {
            // A package-primary document carries no minted `id`/`kind` in its
            // metadata (see `primary_document`), so `identity` is always
            // `None` for it; admit it by the same rule that opened it.
            validate_identity(doc, checkout, &set.home)?;
        } else if identity(doc).as_ref() != Some(checkout) {
            return Err(RecordOpenError::IdentityMismatch);
        }
        Ok(RecordAdmission {
            checkout: checkout.clone(),
            workspace: set.home.clone(),
        })
    }

    pub(crate) fn with_checked<R>(
        &self,
        id: &RecordId,
        f: impl FnOnce(&mut AutoCommit) -> R,
    ) -> Result<R, RecordOpenError> {
        // A checked mutable loan is external to any action frame, exactly as
        // `document_mut`: close the open frame and advance its generation so
        // an operation resolved before this loan is invalidated.
        self.with_action_state(id, |document, history, actions| {
            actions.finish_external(document, history);
            actions.generation = actions
                .generation
                .and_then(|generation| generation.checked_add(1));
            f(document)
        })
    }

    pub(crate) fn read_checked<R>(
        &self,
        id: &RecordId,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        set.not_lent(id)?;
        let document = set
            .open
            .get(id)
            .and_then(|parked| parked.doc.as_ref())
            .ok_or(RecordOpenError::MissingRecord)?;
        Ok(read(document))
    }

    /// One exclusive record admission for the document and owned action frame.
    ///
    /// The record's document, history and action state are lent out of the
    /// set for the frame, so what the frame does may open, read and change
    /// the set's other records (the parts of a chain beside the record it
    /// shows); the record itself answers [`RecordOpenError::Busy`] to
    /// anyone else until the frame returns it.
    pub(crate) fn with_action_state<R>(
        &self,
        id: &RecordId,
        callback: impl FnOnce(
            &mut AutoCommit,
            &mut crate::state::UndoLog,
            &mut crate::bound_dispatch::RecordActionState,
        ) -> R,
    ) -> Result<R, RecordOpenError> {
        let mut loan = {
            let mut set = self.0.try_borrow_mut().map_err(|_| RecordOpenError::Busy)?;
            set.not_lent(id)?;
            let parked = set.open.get_mut(id).ok_or(RecordOpenError::MissingRecord)?;
            let doc = parked.doc.take().ok_or(RecordOpenError::MissingRecord)?;
            let lent = (
                doc,
                std::mem::take(&mut parked.history),
                std::mem::take(&mut parked.action_state),
            );
            set.lent.insert(id.clone());
            RecordLoan {
                store: self,
                id,
                parts: Some(lent),
            }
        };
        let (doc, history, state) = loan.parts.as_mut().ok_or(RecordOpenError::MissingRecord)?;
        Ok(crate::invocation_scope::with_record_loan(
            self,
            id,
            doc,
            |doc| callback(doc, history, state),
        ))
    }

    pub(crate) fn invocation_provenance(
        &self,
        id: &RecordId,
    ) -> Result<
        std::rc::Rc<std::cell::RefCell<crate::invocation_scope::InvocationProvenance>>,
        RecordOpenError,
    > {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        Ok(parked.action_state.invocation_provenance.clone())
    }

    /// The writers this replica has written the open record `id` as, for a
    /// route that publishes this replica's own changes and nobody else's
    /// ([`crate::state::DocumentFrontier::delta`]).
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub(crate) fn writing_actors(
        &self,
        id: &RecordId,
    ) -> Result<crate::state::WritingActors, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        Ok(parked.action_state.writers.borrow().clone())
    }

    pub(crate) fn require_invocation_provenance(
        &self,
        checkout: &RecordCheckout,
    ) -> Result<(), String> {
        self.open_existing_record_checked(checkout)
            .map_err(|error| error.to_string())?;
        let (_, package, workspace) = self
            .history_namespace(&checkout.id)
            .map_err(|error| error.to_string())?;
        let cluster = uuid::Uuid::parse_str(&workspace).map_err(|error| error.to_string())?;
        let route = if checkout.id.is_package_primary() {
            crate::collaboration::RouteSpec::app(package, cluster)
        } else {
            crate::collaboration::RouteSpec::record(package, cluster, checkout.id.as_str())
        };
        self.with_action_state(&checkout.id, |document, _, state| {
            state
                .invocation_provenance
                .borrow_mut()
                .require(route, document)
        })
        .map_err(|error| error.to_string())?
    }

    /// Generation of the record-owned action frame, for retained callers that
    /// need to invalidate a previously resolved operation.
    ///
    /// # Errors
    /// Returns `Busy` if the record set is borrowed, or `MissingRecord` if the record is not open.
    pub fn action_generation(&self, id: &RecordId) -> Result<Option<u64>, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        Ok(parked.action_state.generation)
    }

    /// Read canonical edit state without opening an exclusive action loan.
    /// Admission generations advance even on idle callbacks; deferred input
    /// instead follows document changes and actual undo/redo transitions.
    pub(crate) fn document_edit_stamp(
        &self,
        id: &RecordId,
        lifetime: uuid::Uuid,
    ) -> Result<crate::bound_dispatch::DocumentEditStamp, RecordOpenError> {
        let set = self.0.try_borrow().map_err(|_| RecordOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(RecordOpenError::MissingRecord)?;
        let document = parked.doc.as_ref().ok_or(RecordOpenError::Busy)?;
        if parked
            .action_state
            .generation
            .is_none_or(|value| value == u64::MAX)
            || parked.history.revision() == u64::MAX
        {
            return Err(RecordOpenError::Busy);
        }
        Ok(
            crate::bound_dispatch::DocumentEditStamp::for_owned_document(
                document,
                lifetime,
                parked.history.revision(),
            ),
        )
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
    pub(crate) fn merge_remote_document(
        &self,
        checkout: &RecordCheckout,
        mutation: collab_types::DocumentMutation,
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        // The workspace's record may still be decoding off the frame when its
        // route's committed history arrives: take it in first, as every open
        // of it does, so the history lands on it.
        self.0
            .try_borrow_mut()
            .map_err(|_| RecordOpenError::Busy.to_string())?
            .take_in_opening(&checkout.id, true)
            .map_err(|error| error.to_string())?;
        let identity = self.replica_identity_stamp(checkout)?;
        let merged = self.with_action_state(&checkout.id, |doc, history, actions| {
            let mut local = doc.clone();
            actions.commit_for_save(&mut local);
            let before = local.get_heads();
            let mut candidate;
            // The heads the payload names: a snapshot re-delivered unchanged
            // still tells the route that these are the sender's own.
            let delivered = match &mutation {
                collab_types::DocumentMutation::Snapshot(bytes) => {
                    let mut incoming = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
                    // The host's empty document for a record a member is
                    // creating carries nothing to merge, and no identity.
                    if incoming.get_heads().is_empty() {
                        return Ok((false, Vec::new()));
                    }
                    identity.validate(&incoming)?;
                    let heads = incoming.get_heads();
                    // Merging copies every change the receiving side lacks, so
                    // the local changes go into the snapshot rather than the
                    // other way round: a late joiner's replica is a few
                    // changes next to a workspace's checkpoint. Every ancestor of
                    // the heads both sides know is already in the snapshot.
                    let shared: Vec<automerge::ChangeHash> = heads
                        .iter()
                        .copied()
                        .filter(|head| local.get_change_meta_by_hash(head).is_some())
                        .collect();
                    if shared.len() == heads.len() {
                        candidate = local.clone();
                    } else if !local.get_missing_deps(&[]).is_empty() {
                        // Changes the replica received before the ones they
                        // build on wait in it, and only there: copying the
                        // replica's changes into the snapshot would leave
                        // them behind, so the snapshot's come to the replica.
                        candidate = local.clone();
                        candidate
                            .load_incoming(&incoming.save_after(&[]))
                            .map_err(|error| error.to_string())?;
                    } else {
                        // Merging walks back to what both sides hold. Saving
                        // after the shared heads alone would re-encode the
                        // whole local history when the snapshot is ahead.
                        incoming.set_actor(local.get_actor().clone());
                        incoming
                            .merge_incoming(&mut local)
                            .map_err(|error| error.to_string())?;
                        candidate = incoming;
                    }
                    heads
                }
                collab_types::DocumentMutation::Incremental(bytes) => {
                    candidate = local.clone();
                    crate::history_mode::routes::load_incremental(&mut candidate, bytes)
                        .map_err(|error| error.to_string())?;
                    Vec::new()
                }
            };
            identity.validate(&candidate)?;
            let changed = candidate.get_heads() != before;
            // What the route delivered. A head stands for all of its
            // ancestors only in an ancestry frontier; this one names each
            // delivered change, so the merge's applied changes are listed,
            // and so are the ones still waiting for history this replica
            // lacks. Neither is ever sent back on the route that brought it.
            let mut observed = candidate
                .get_changes(&before)
                .into_iter()
                .map(|change| change.hash())
                .collect::<Vec<_>>();
            observed.extend(delivered);
            if let collab_types::DocumentMutation::Incremental(bytes) = &mutation {
                observed.extend(crate::state::changes_beyond(&mut candidate, bytes));
            }
            let mut seen = std::collections::HashSet::new();
            observed.retain(|hash| seen.insert(*hash));
            if changed {
                // Finish the local span against LOCAL heads, never the merged
                // heads: undoing it must leave the remote author's edits intact.
                actions.finish_external(&mut local, history);
                history.rebase(&mut candidate);
                actions.generation = actions.generation.and_then(|value| value.checked_add(1));
            }
            // Keep valid changes with dependencies still in flight, too.
            let missing = candidate.get_missing_deps(&[]);
            if !missing.is_empty() {
                // Shown until they arrive; a record that keeps waiting never
                // received a change another replica built on.
                tracing::debug!(
                    record = %checkout.id,
                    missing = ?missing.iter().take(4).map(|hash| hash.to_string()).collect::<Vec<_>>(),
                    "remote changes wait for changes this replica has not received"
                );
            }
            *doc = candidate;
            Ok((changed, observed))
        })
        .map_err(|error| error.to_string())?;
        if let Ok((true, _)) = &merged {
            self.arrived(&checkout.id);
            self.wake_readers();
        }
        merged
    }

    /// Merge a peer's full document into an open record, exactly as a
    /// collaborator's state arriving over sync is merged: validated first,
    /// and without moving the local author's undo history onto the peer's
    /// edits. Returns whether anything changed.
    ///
    /// # Errors
    /// Returns why the peer's document was refused; the record is unchanged.
    pub fn merge_peer_document(
        &self,
        checkout: &RecordCheckout,
        document: Vec<u8>,
    ) -> Result<bool, String> {
        self.merge_remote_document(checkout, collab_types::DocumentMutation::Snapshot(document))
            .map(|(changed, _)| changed)
    }

    /// Merge another replica's incremental changes (what
    /// [`Self::replication_delta`] answers there) into the open record, as
    /// the workspace's broker delivers one window's changes to the others on the
    /// record. Whether anything changed.
    ///
    /// # Errors
    /// The record is not open here or the changes do not merge.
    pub fn merge_peer_changes(
        &self,
        checkout: &RecordCheckout,
        changes: Vec<u8>,
    ) -> Result<bool, String> {
        self.merge_remote_document(
            checkout,
            collab_types::DocumentMutation::Incremental(changes),
        )
        .map(|(changed, _)| changed)
    }

    /// Withdraw from the open record `id` the changes `refused` names, which
    /// the workspace refused, with every local change made on top of them, and
    /// note one [`RecordRefusal`] per action they carried, saying `reason`
    /// (for a person), for the app to take ([`Self::take_refusals`]).
    ///
    /// A refused change is shared state nowhere but here, and a peer holds
    /// back every change that depends on one it never receives: kept, it
    /// would show as sent forever and silence everything written after it.
    /// The document becomes what it was without them, saved whole; local
    /// undo starts over, since its steps may name withdrawn changes. A
    /// replication frontier past them must be [`rebased`] onto the heads
    /// this returns. Returns `None` when nothing was withdrawn: the record
    /// holds none of them, or they are its creation (still noted).
    ///
    /// [`rebased`]: crate::state::DocumentFrontier::rebase
    ///
    /// # Errors
    /// The record is not open here or its owner is busy, or the document
    /// does not rebuild.
    pub fn withdraw_refused(
        &self,
        id: &RecordId,
        refused: &[automerge::ChangeHash],
        reason: &str,
    ) -> Result<Option<Vec<automerge::ChangeHash>>, String> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| RecordOpenError::Busy.to_string())?;
        let set = &mut *set;
        let portable = set
            .portable
            .as_ref()
            .map(|cache| (cache, portable::key(&set.package_id, &set.home, id)));
        let parked = set
            .open
            .get_mut(id)
            .ok_or_else(|| RecordOpenError::MissingRecord.to_string())?;
        let Parked {
            doc,
            autosave,
            history,
            action_state,
            ..
        } = parked;
        let doc = doc
            .as_mut()
            .ok_or_else(|| RecordOpenError::MissingRecord.to_string())?;
        let withdrawn = crate::invocation_scope::with_record_loan(
            self,
            id,
            doc,
            |doc| -> Result<Option<Vec<automerge::ChangeHash>>, String> {
                if !refused
                    .iter()
                    .any(|hash| doc.get_change_meta_by_hash(hash).is_some())
                {
                    return Ok(None);
                }
                // What was typed so far closes as its own step first.
                action_state.finish_external(doc, history);
                let (kept_heads, refusals) = withdrawal(doc, refused, reason);
                action_state.refusals.extend(refusals);
                if kept_heads.is_empty() {
                    // The record's own creation was refused: it stays as this
                    // device made it, a record nobody else holds, rather than
                    // one with no identity. Nothing said of it holds.
                    action_state.notices.held.clear();
                    return Ok(None);
                }
                // The withdrawn changes left this copy but not the workspace:
                // its relay holds them under this copy's writer and the
                // writers derived from it (`bound_dispatch::document_writer`,
                // `author_writer`), numbered. Written on under those writers,
                // the rebuilt document would number its next changes the
                // same again, and a record holding two changes under one
                // writer and sequence reads whole nowhere. So the copy
                // writes under a writer of its own from here; what it wrote
                // before and still holds stays its own to publish.
                let before = doc.get_actor().clone();
                let device = crate::history_source::authors::DeviceActors::of(&before);
                {
                    let mut writers = action_state.writers.borrow_mut();
                    writers.note(before.clone());
                    for change in doc.get_changes_meta(&[]) {
                        if device.wrote(&change.actor.to_hex_string()) {
                            writers.note(change.actor.into_owned());
                        }
                    }
                }
                let actor = automerge::ActorId::random();
                if let Some((cache, key)) = &portable {
                    cache.claim(key, actor.to_bytes().to_vec())?;
                }
                let mut rebuilt = doc
                    .fork_at(&kept_heads)
                    .map_err(|error| error.to_string())?;
                rebuilt.set_actor(actor);
                history.restart(&mut rebuilt);
                action_state.generation = action_state
                    .generation
                    .and_then(|value| value.checked_add(1));
                *doc = rebuilt;
                action_state.notices.withdrawn(doc);
                Ok(Some(doc.get_heads()))
            },
        )?;
        if withdrawn.is_some()
            && let Some(writer) = autosave.as_mut()
        {
            // What was saved holds the withdrawn changes: only a whole save
            // replaces it.
            writer.request_checkpoint(doc);
        }
        Ok(withdrawn)
    }

    /// Say `text` of what was just done to the open record `id` once the
    /// workspace it is shared in keeps it (`WorkspaceNotices`): at once when no
    /// host replicates it, never when the workspace refuses it. The pending
    /// action is committed first, so the change it names is among the heads
    /// held.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn hold_notice(&self, id: &RecordId, text: String) -> Result<(), RecordOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let heads = doc.get_heads();
            actions.notices.hold(text, heads);
        })
    }

    /// The workspace's host answered every commit of record `id` it published:
    /// what was held for them is said ([`Self::take_said`]).
    pub fn workspace_settled(&self, id: &RecordId) {
        if let Ok(mut set) = self.0.try_borrow_mut()
            && let Some(parked) = set.open.get_mut(id)
        {
            parked.action_state.notices.settled();
        }
    }

    /// What to say now of record `id` ([`Self::hold_notice`]), each once.
    pub fn take_said(&self, id: &RecordId) -> Vec<String> {
        let Ok(mut set) = self.0.try_borrow_mut() else {
            return Vec::new();
        };
        set.open
            .get_mut(id)
            .map(|parked| std::mem::take(&mut parked.action_state.notices.said))
            .unwrap_or_default()
    }

    /// The refusals of record `id` noted since the last call
    /// ([`Self::withdraw_refused`]).
    pub fn take_refusals(&self, id: &RecordId) -> Vec<RecordRefusal> {
        let Ok(mut set) = self.0.try_borrow_mut() else {
            return Vec::new();
        };
        set.open
            .get_mut(id)
            .map(|parked| std::mem::take(&mut parked.action_state.refusals))
            .unwrap_or_default()
    }

    /// Changes to the open record `id` that `frontier` has not covered, for a
    /// host replicating open records itself (a browser page over the relay).
    /// The record goes whole, another replica's changes included; a route
    /// publishes its own alone ([`Self::own_delta`]).
    /// The pending action is committed first, as for a save. Advance the
    /// frontier with the returned heads only once the delta was accepted.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn replication_delta(
        &self,
        id: &RecordId,
        frontier: &mut crate::state::DocumentFrontier,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), RecordOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let (delta, heads) = frontier.pending_mirror_delta(doc);
            if delta.is_empty() {
                actions.notices.replicating();
            } else {
                actions.notices.sent(&heads);
            }
            (delta, heads)
        })
    }

    /// The changes to the open record `id` this replica made and `frontier`
    /// has not published yet, for a route, which publishes its own device's
    /// changes and never another replica's
    /// ([`crate::state::DocumentFrontier::pending_delta`]). The pending action
    /// is committed first, as for a save. Advance the frontier with the
    /// returned heads only once the delta was accepted.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn own_delta(
        &self,
        id: &RecordId,
        frontier: &mut crate::state::DocumentFrontier,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), RecordOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let own = actions.writers.borrow().clone();
            let (delta, heads) = frontier.pending_delta(doc, &own);
            if delta.is_empty() {
                actions.notices.replicating();
            } else {
                actions.notices.sent(&heads);
            }
            (delta, heads)
        })
    }

    /// Bring the open record `id` up with `migrate`, and note the actors it
    /// wrote as among this replica's own: a migration is one housekeeping
    /// change whose actor is derived from the document rather than being this
    /// device's, so a route publishing this replica's own changes
    /// ([`crate::state::DocumentFrontier::pending_delta`]) sends it too.
    ///
    /// # Errors
    /// The record is not open here, its owner is busy, or `migrate` refused.
    pub fn migrate<T>(
        &self,
        id: &RecordId,
        migrate: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_action_state(id, |doc, _, actions| {
            let before = doc.get_heads();
            let outcome = migrate(doc);
            for change in doc.get_changes(&before) {
                actions.writers.borrow_mut().note(change.actor_id().clone());
            }
            outcome
        })
        .map_err(|error| error.to_string())?
    }

    /// The open record `id` as a whole-document snapshot with its heads, for
    /// a replica's first publication.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn replication_snapshot(
        &self,
        id: &RecordId,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), RecordOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let heads = doc.get_heads();
            actions.notices.sent(&heads);
            (doc.save(), heads)
        })
    }

    /// Merge a replica's mutation into the open record, validating the
    /// record's identity on a private copy first; local undo stays local.
    /// Returns whether the document changed and the change hashes the
    /// mutation carried, for the replicating frontier's `observe`.
    ///
    /// # Errors
    /// The bytes are not a valid mutation of this record, or it is not open.
    pub fn merge_replica(
        &self,
        checkout: &RecordCheckout,
        mutation: collab_types::DocumentMutation,
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        self.merge_remote_document(checkout, mutation)
    }

    /// Do something with a record's document. `None` if the record is not open here.
    pub fn with<R>(&self, id: &RecordId, f: impl FnOnce(&mut AutoCommit) -> R) -> Option<R> {
        let mut set = self.0.borrow_mut();
        set.document_mut(id)
            .map(|document| crate::invocation_scope::with_record_loan(self, id, document, f))
    }

    /// Whether the store takes a write now: not while something reads or
    /// changes it (a frame's read of a record holds it).
    #[must_use]
    pub fn takes_writes(&self) -> bool {
        self.0.try_borrow_mut().is_ok()
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
        let mut set = self.0.try_borrow_mut().ok()?;
        let (doc, history) = set.document_and_history_mut(id)?;
        Some(f(doc, history))
    }

    /// Inspect committed history without turning a pending burst into a boundary.
    pub fn read_history<R>(
        &self,
        id: &RecordId,
        read: impl FnOnce(&AutoCommit, &crate::state::UndoLog) -> R,
    ) -> Option<R> {
        let set = self.0.try_borrow().ok()?;
        set.not_lent(id).ok()?;
        let parked = set.open.get(id)?;
        Some(read(parked.doc.as_ref()?, &parked.history))
    }

    /// Every record this store holds.
    pub fn open_records(&self) -> Vec<RecordCheckout> {
        self.0.borrow().open_records()
    }

    /// The open records that belong to another's tree ([`RecordId::tree`]),
    /// ready to read; none while a frame holds the store.
    #[must_use]
    pub fn open_tree_records(&self) -> Vec<RecordCheckout> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        set.open
            .iter()
            .filter(|(id, parked)| parked.doc.is_some() && id.tree().is_some())
            .map(|(id, parked)| RecordCheckout::new(id.clone(), parked.kind.clone()))
            .collect()
    }

    /// Offer every open record to its writer. Once per idle tick.
    ///
    /// # Errors
    /// A record's cache reports that it could not take the document.
    pub fn tick(&self) -> Result<(), String> {
        self.0.borrow_mut().tick_open()
    }

    /// Write out every open record and wait for it to land.
    ///
    /// # Errors
    /// Each record's writer that reports a storage failure, by record.
    pub fn flush(&self) -> Result<(), String> {
        self.0.borrow_mut().flush_open()
    }
}

/// Whether the document carries its record metadata map.
fn record_metadata_present(doc: &AutoCommit) -> Result<bool, String> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => Ok(false),
        ObjectAt::Object(_) => Ok(true),
        ObjectAt::Invalid => Err("conflicting record metadata".into()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReplicaIdentityStamp {
    checkout: RecordCheckout,
    workspace: String,
    metadata: bool,
    home: Option<String>,
    /// The workspace the document names, as [`workspace_scope`] reads it.
    scope: Option<String>,
}

impl ReplicaIdentityStamp {
    fn capture(
        doc: &AutoCommit,
        checkout: &RecordCheckout,
        workspace: &str,
    ) -> Result<Self, String> {
        validate_identity(doc, checkout, workspace).map_err(|error| error.to_string())?;
        let metadata = record_metadata_present(doc)?;
        if !checkout.id.is_package_primary() && !metadata {
            return Err("record metadata is missing".into());
        }
        Ok(Self {
            checkout: checkout.clone(),
            workspace: workspace.to_string(),
            metadata,
            home: home(doc),
            scope: workspace_scope(doc),
        })
    }

    pub(crate) fn validate(&self, doc: &AutoCommit) -> Result<(), String> {
        validate_identity(doc, &self.checkout, &self.workspace)
            .map_err(|error| error.to_string())?;
        if record_metadata_present(doc)? != self.metadata
            || home(doc) != self.home
            || workspace_scope(doc) != self.scope
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
/// live one, park it and check the named one out. `tilo_ui::App` calls this
/// once per frame, before `sync`, so a widget that opened a file last
/// frame is editing that file's document by the time it next reads state.
/// The checkout it returns is the caller's cue to re-announce its
/// collaboration channel — a checkout is a change of address.
pub fn tick(
    set: &mut RecordSet,
    state: &mut crate::state::State,
    workspace: Option<&mut AutoCommit>,
    widget: &dyn crate::widget::Widget,
) -> Option<RecordCheckout> {
    let want = crate::record::capability::record_intent(widget)?;
    set.checkout(state, workspace, &want).then_some(want)
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod identity_tests;

mod arrival;
mod canonical_read;
mod opening;
pub use opening::RecordOpening;
#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod named_creation_tests;
#[cfg(test)]
mod replica_merge_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod portable_tests;
