//! Transfer: the explicit verb that takes a record across a room boundary.
//!
//! A record id names a document only inside the room that hydrates it
//! ([`super::room_scope`]), so nothing crosses rooms by reference. Transfer
//! is the counterpart of [`super::room::duplicate`] for a document that
//! already has content: the destination room receives the record's whole
//! change graph under a fresh id of its own, re-scoped to that room, with
//! the source id kept in its `formerly` list as provenance. Nothing ever
//! looks a record up through that list.
//!
//! Moving is transfer followed by [`retire`]: the source record gets one
//! last change saying it left, which stays in its history the way a
//! deletion does, and it is never opened as a live document again
//! ([`super::RecordOpenError::Retired`]). Copying is transfer alone: the
//! source keeps going and the two histories diverge from here.
//!
//! An app that keeps one document for itself in a room ([`crate::persist`])
//! transfers the same way ([`Origin::AppDocument`]): its document arrives
//! as a record, and a moved one is retired where it was.
//!
//! Every function works on the durable store a room's app profile keeps,
//! through the same keys, leases and validation the record owners use. A
//! write needs the key's exclusive lease, so it fails with
//! [`TransferError::Busy`] while an app process still has the record open;
//! a caller waits for that process to go rather than racing its writer.

use std::rc::Rc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, ScalarValue, Value};
pub use durable_store::DurableStore;
use durable_store::{StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;

use super::{
    FORMERLY, ObjectAt, RecordCheckout, RecordId, RecordKind, RecordOpenError, StringAt, identity,
    meta_obj, meta_ro, object_at, string_at, validate_identity,
};

/// The kind an app's own document ([`Origin::AppDocument`]) takes when it
/// arrives in another room as a record.
pub const APP_DOCUMENT_KIND: &str = "tilo.app-document";

/// Key, in a record's meta map, marking a record that left its room.
pub(super) const RETIRED: &str = "retired";

/// Where a moved record went and who moved it, as its retirement records.
/// Anyone who can read the old record learns only these ids; whether they
/// can see the destination is for their own desktop to say.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Successor {
    /// The room the record now lives in.
    pub room: String,
    /// Its id there.
    pub record: String,
    /// The person who moved it.
    pub by: String,
}

/// Why a record is retired, as stamped into its last change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// Moved to another room; the successor there carries its history.
    /// `None` only for a stamp that no longer says where.
    Moved(Option<Successor>),
    /// Imported for a transfer that did not complete; nothing ever opened it.
    Abandoned,
}

impl Retirement {
    fn reason(&self) -> &'static str {
        match self {
            Self::Moved(_) => "moved",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Open(#[from] RecordOpenError),
    #[error(transparent)]
    Store(#[from] app_state::StoreError),
    /// An app process still holds the record's writer.
    #[error("the record is still open in an app")]
    Busy,
    #[error("{0}")]
    NotTransferable(&'static str),
    #[error("automerge: {0}")]
    Automerge(String),
}

impl From<durable_store::StoreError> for TransferError {
    fn from(error: durable_store::StoreError) -> Self {
        match error {
            durable_store::StoreError::Busy(_) => Self::Busy,
            other => Self::Store(other.into()),
        }
    }
}

/// The retirement stamped into `doc`, when it has left its room.
pub fn retirement(doc: &AutoCommit) -> Option<Retirement> {
    let meta = meta_ro(doc)?;
    let stamp = match object_at(doc, &meta, RETIRED, ObjType::Map) {
        ObjectAt::Object(stamp) => stamp,
        ObjectAt::Missing => {
            // Present in some other shape: retired all the same.
            return (!matches!(string_at(doc, &meta, RETIRED), StringAt::Missing))
                .then_some(Retirement::Moved(None));
        }
        ObjectAt::Invalid => return Some(Retirement::Moved(None)),
    };
    let field = |key: &str| match string_at(doc, &stamp, key) {
        StringAt::String(value) => Some(value),
        StringAt::Missing | StringAt::Invalid => None,
    };
    if field("reason").as_deref() == Some("abandoned") {
        return Some(Retirement::Abandoned);
    }
    let successor = match (field("room"), field("record"), field("by")) {
        (Some(room), Some(record), Some(by)) => Some(Successor { room, record, by }),
        _ => None,
    };
    Some(Retirement::Moved(successor))
}

/// Whether `doc` is a record that left its room.
pub fn is_retired(doc: &AutoCommit) -> bool {
    retirement(doc).is_some()
}

/// Stamp `doc` retired, in one change of its own, unless it already is.
/// The shell does this to a record at rest; an app does it to the live
/// document it shows, so the stamp travels to everyone holding that record
/// over its normal route.
///
/// # Errors
/// A document whose metadata cannot hold the stamp.
pub fn stamp_retired(doc: &mut AutoCommit, why: &Retirement) -> Result<(), TransferError> {
    if is_retired(doc) {
        return Ok(());
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    let meta = meta_obj(doc).ok_or(RecordOpenError::IdentityMismatch)?;
    let stamp = doc
        .put_object(&meta, RETIRED, ObjType::Map)
        .map_err(automerge)?;
    doc.put(&stamp, "reason", why.reason()).map_err(automerge)?;
    if let Retirement::Moved(Some(successor)) = why {
        doc.put(&stamp, "room", successor.room.as_str())
            .map_err(automerge)?;
        doc.put(&stamp, "record", successor.record.as_str())
            .map_err(automerge)?;
        doc.put(&stamp, "by", successor.by.as_str())
            .map_err(automerge)?;
    }
    doc.commit_with(CommitOptions::default().with_message(match why {
        Retirement::Moved(_) => "tilo.record.moved-out".to_owned(),
        Retirement::Abandoned => "tilo.record.transfer-abandoned".to_owned(),
    }));
    Ok(())
}

/// What a transfer takes out of the source room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// One of the app's records.
    Record(RecordCheckout),
    /// The document an app keeps for itself in a room through
    /// [`crate::persist`]. It has no record identity of its own there, and
    /// arrives in the destination as a record of this kind.
    AppDocument(RecordKind),
}

impl Origin {
    fn key(&self, package: &str, room: &str) -> Result<StorageKey, TransferError> {
        match self {
            Self::Record(checkout) => key(package, room, &checkout.id),
            Self::AppDocument(_) => {
                app_state::validate_record_namespace(package, room)?;
                Ok(StorageKey::package(package))
            }
        }
    }

    /// Refuse anything but this origin's live document in `room`.
    ///
    /// # Errors
    /// A document that is not this origin's live document there.
    pub fn validate(&self, doc: &AutoCommit, room: &str) -> Result<(), TransferError> {
        match self {
            Self::Record(checkout) => Ok(validate_identity(doc, checkout, room)?),
            Self::AppDocument(_) if is_retired(doc) => Err(RecordOpenError::Retired.into()),
            Self::AppDocument(_) if identity(doc).is_some() => {
                Err(RecordOpenError::IdentityMismatch.into())
            }
            Self::AppDocument(_) => Ok(()),
        }
    }

    /// The kind the document has, or takes once it arrives as a record.
    pub fn kind(&self) -> &RecordKind {
        match self {
            Self::Record(checkout) => &checkout.kind,
            Self::AppDocument(kind) => kind,
        }
    }
}

fn key(package: &str, room: &str, id: &RecordId) -> Result<StorageKey, TransferError> {
    if id.is_package_primary() {
        return Err(TransferError::NotTransferable(
            "a room's own package document never leaves its room",
        ));
    }
    let record = id.storage_key();
    app_state::validate_record_identity(package, room, &record)?;
    Ok(StorageKey::record(package, room, record))
}

fn decode(store: &Rc<dyn DurableStore>, key: &StorageKey) -> Result<AutoCommit, TransferError> {
    let stored = store
        .read(key)?
        .filter(|stored| !stored.bytes.is_empty())
        .ok_or(RecordOpenError::MissingRecord)?;
    let label = store
        .local_path(key)
        .unwrap_or_else(|| std::path::PathBuf::from(key.to_string()));
    let (doc, _) = app_state::decode_canonical(&stored.bytes, false, &label)?;
    Ok(doc)
}

/// Read `origin` as `package` last made it durable in `room`, without
/// taking its writer: the app showing it may still be running.
///
/// # Errors
/// A missing, retired, foreign-room or malformed document, or storage failure.
pub fn read(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    origin: &Origin,
) -> Result<AutoCommit, TransferError> {
    let doc = decode(store, &origin.key(package, room)?)?;
    origin.validate(&doc, room)?;
    Ok(doc)
}

/// A live record of `package` in `room` by its id alone, whatever its kind:
/// what another device asks for when all it knows is the id.
///
/// # Errors
/// A missing, retired, foreign-room or malformed record, or storage failure.
pub fn read_by_id(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    id: &RecordId,
) -> Result<AutoCommit, TransferError> {
    let doc = decode(store, &key(package, room, id)?)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, room)?;
    Ok(doc)
}

/// Verify `bytes` another device sent as record `id` of `room`, and keep it
/// here: installed when new, merged when this device already holds it. The
/// bytes are only trusted as far as they prove to be that live record.
///
/// # Errors
/// Bytes that are not a live record `id` scoped to `room`, or storage failure.
pub fn adopt_fetched(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    id: &RecordId,
    bytes: &[u8],
) -> Result<RecordCheckout, TransferError> {
    let label = std::path::PathBuf::from(format!("fetched {id}"));
    let (mut doc, _) = app_state::decode_canonical(bytes, false, &label)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, room)?;
    match install(store, package, room, &mut doc) {
        Err(TransferError::Open(RecordOpenError::AlreadyExists)) => {
            catch_up(store, package, room, &checkout, &doc)?;
            Ok(checkout)
        }
        other => other,
    }
}

/// Identity keys a record's meta map may hold; anything else could claim
/// more than the one record the document is.
const IMPORTABLE_META: [&str; 7] = ["id", "kind", "home", super::ROOM, FORMERLY, "refs", "blobs"];

/// Check `bytes` offered to become record `id` of `room` with their whole
/// history, before anything keeps them: at most `max_bytes`, a complete
/// document whose identity is exactly that freshly minted record scoped to
/// that room, live, holding no room index and no references to other
/// records, and naming earlier ids only as provenance.
///
/// # Errors
/// Bytes that are too large, malformed, or not exactly that record.
pub fn validate_import(
    bytes: &[u8],
    room: &str,
    id: &RecordId,
    max_bytes: usize,
) -> Result<(AutoCommit, RecordCheckout), TransferError> {
    if bytes.len() > max_bytes {
        return Err(TransferError::NotTransferable("the document is too large"));
    }
    let minted = id.as_str().strip_prefix("uuid:").is_some_and(|text| {
        uuid::Uuid::parse_str(text).is_ok_and(|uuid| uuid.hyphenated().to_string() == text)
    });
    if !minted || RecordId::parse(id.as_str()).as_ref() != Some(id) {
        return Err(TransferError::NotTransferable(
            "a document joins a room only as a newly minted record",
        ));
    }
    let label = std::path::PathBuf::from(format!("imported {id}"));
    let (doc, _) = app_state::decode_canonical(bytes, false, &label)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, room)?;
    if super::room_scope(&doc).as_deref() != Some(room) {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    if doc
        .get(automerge::ROOT, super::INDEX)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(TransferError::NotTransferable(
            "a document listing other records cannot join a room",
        ));
    }
    let meta = meta_ro(&doc).ok_or(RecordOpenError::IdentityMismatch)?;
    if doc
        .keys(&meta)
        .any(|key| !IMPORTABLE_META.contains(&key.as_str()))
    {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    if !super::references(&doc).is_empty()
        || matches!(object_at(&doc, &meta, "refs", ObjType::List), ObjectAt::Object(list) if doc.length(&list) != 0)
    {
        return Err(TransferError::NotTransferable(
            "a document referring to other records cannot join a room",
        ));
    }
    let formerly = match object_at(&doc, &meta, FORMERLY, ObjType::List) {
        ObjectAt::Object(list) => doc.length(&list),
        ObjectAt::Missing => 0,
        ObjectAt::Invalid => return Err(RecordOpenError::IdentityMismatch.into()),
    };
    let provenance = provenance(&doc);
    if formerly > super::MAX_FORMER_IDS
        || provenance.len() != formerly
        || provenance.iter().any(|former| former == id)
    {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    Ok((doc, checkout))
}

/// [`read`] as of `heads`: what the document held before its app stamped
/// it retired, which is everything a move still has to bring along.
///
/// # Errors
/// As [`read`], and heads the stored document does not contain.
pub fn read_until(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    origin: &Origin,
    heads: &[ChangeHash],
) -> Result<AutoCommit, TransferError> {
    let mut stored = decode(store, &origin.key(package, room)?)?;
    let doc = stored
        .fork_at(heads)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    origin.validate(&doc, room)?;
    Ok(doc)
}

/// The destination's copy of `source`, read from `origin` in `from_room`:
/// its whole history, one more change giving it `target`'s id and
/// `to_room`'s scope, and a record's origin id kept as provenance. `source`
/// is not changed.
///
/// # Errors
/// A source that is not `origin`'s live document, or a transfer into the
/// room it is already in.
pub fn rehome(
    source: &AutoCommit,
    origin: &Origin,
    from_room: &str,
    to_room: &str,
    target: &RecordId,
) -> Result<AutoCommit, TransferError> {
    if target.is_package_primary()
        || matches!(origin, Origin::Record(checkout) if checkout.id.is_package_primary())
    {
        return Err(TransferError::NotTransferable(
            "a room's own package document never leaves its room",
        ));
    }
    if RecordId::parse(target.as_str()).as_ref() != Some(target)
        || matches!(origin, Origin::Record(checkout) if &checkout.id == target)
    {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    origin.validate(source, from_room)?;
    if from_room == to_room || to_room.is_empty() || from_room.is_empty() {
        return Err(TransferError::NotTransferable(
            "the record is already in that room",
        ));
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    // A fork continues the same change graph under a writer of its own.
    let mut doc = source.clone().fork();
    match origin {
        Origin::Record(checkout) => {
            let meta = meta_obj(&mut doc).ok_or(RecordOpenError::IdentityMismatch)?;
            doc.put(&meta, "id", target.as_str()).map_err(automerge)?;
            doc.put(&meta, super::ROOM, to_room).map_err(automerge)?;
            let formerly = match object_at(&doc, &meta, FORMERLY, ObjType::List) {
                ObjectAt::Object(list) => list,
                ObjectAt::Missing => doc
                    .put_object(&meta, FORMERLY, ObjType::List)
                    .map_err(automerge)?,
                ObjectAt::Invalid => return Err(RecordOpenError::IdentityMismatch.into()),
            };
            let len = doc.length(&formerly);
            doc.insert(&formerly, len, checkout.id.as_str())
                .map_err(automerge)?;
        }
        // The app's own document becomes a record the moment it leaves: it
        // was minted in the source room and is now scoped to the destination.
        Origin::AppDocument(kind) => {
            if !super::init_scoped(&mut doc, target, kind, from_room, Some(to_room)) {
                return Err(RecordOpenError::IdentityMismatch.into());
            }
        }
    }
    doc.commit_with(CommitOptions::default().with_message("tilo.record.transfer".to_owned()));
    let arrived = RecordCheckout::new(target.clone(), origin.kind().clone());
    validate_identity(&doc, &arrived, to_room)?;
    Ok(doc)
}

/// The changes `source` holds beyond the `known` heads of an earlier read:
/// what its writer committed after that read.
pub fn changes_since(source: &AutoCommit, known: &[ChangeHash]) -> usize {
    source.clone().get_changes(known).len()
}

/// Hold `key`'s writer for one write. Busy while an app has it open.
fn writer(
    store: &Rc<dyn DurableStore>,
    key: &StorageKey,
) -> Result<app_state::DocOwner, TransferError> {
    app_state::DocOwner::acquire(Rc::clone(store), key.clone()).map_err(|error| match error {
        app_state::StoreError::WriterBusy { .. } => TransferError::Busy,
        other => TransferError::Store(other),
    })
}

/// Whether no app process holds `origin`'s writer in `room` any more: the
/// point after which nothing more can arrive from it.
///
/// # Errors
/// An invalid key or storage failure.
pub fn writer_released(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    origin: &Origin,
) -> Result<bool, TransferError> {
    match writer(store, &origin.key(package, room)?) {
        Ok(_) => Ok(true),
        Err(TransferError::Busy) => Ok(false),
        Err(error) => Err(error),
    }
}

fn persist(
    store: &Rc<dyn DurableStore>,
    key: &StorageKey,
    doc: &mut AutoCommit,
) -> Result<(), TransferError> {
    store.write(key, WriteOp::Replace(doc.save()), None)?;
    store.flush(Some(key))?;
    Ok(())
}

/// Make `doc` (from [`rehome`]) a record of `package` in `room`. Refuses to
/// overwrite anything already stored under its id.
///
/// # Errors
/// A document not scoped to `room`, an occupied id, or storage failure.
pub fn install(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    doc: &mut AutoCommit,
) -> Result<RecordCheckout, TransferError> {
    let checkout = identity(doc).ok_or(RecordOpenError::IdentityMismatch)?;
    install_as(store, package, room, checkout, doc)
}

/// [`install`] a document that arrived for `checkout`, including the room's
/// package-primary record, whose identity the document does not carry.
///
/// # Errors
/// A document that is not `checkout` in `room`, an occupied id, or storage
/// failure.
pub fn install_as(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    checkout: RecordCheckout,
    doc: &mut AutoCommit,
) -> Result<RecordCheckout, TransferError> {
    validate_identity(doc, &checkout, room)?;
    let key = key(package, room, &checkout.id)?;
    let _writer = writer(store, &key)?;
    if store
        .read(&key)?
        .is_some_and(|stored| !stored.bytes.is_empty())
    {
        return Err(RecordOpenError::AlreadyExists.into());
    }
    persist(store, &key, doc)?;
    Ok(checkout)
}

/// Merge every change `source` holds that `target` in `room` lacks — the
/// edits its old writer made after the transfer read it. Deterministic:
/// it is a CRDT merge, so the order it runs in cannot change the result.
/// Returns how many changes arrived.
///
/// # Errors
/// [`TransferError::Busy`] while the destination record is open in an app,
/// a missing or foreign record, or storage failure.
pub fn catch_up(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    target: &RecordCheckout,
    source: &AutoCommit,
) -> Result<usize, TransferError> {
    let key = key(package, room, &target.id)?;
    let _writer = writer(store, &key)?;
    let mut doc = decode(store, &key)?;
    validate_identity(&doc, target, room)?;
    let arriving = doc.clone().get_changes_added(&mut source.clone()).len();
    if arriving == 0 {
        return Ok(0);
    }
    doc.merge_incoming(&mut source.clone())
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    validate_identity(&doc, target, room)?;
    persist(store, &key, &mut doc)?;
    Ok(arriving)
}

/// Stamp `origin` in `room` as retired: one last change in its own history,
/// after which it is never opened as a live document again.
///
/// # Errors
/// [`TransferError::Busy`] while an app still holds the document, a missing
/// or foreign document, or storage failure.
pub fn retire(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    origin: &Origin,
    why: &Retirement,
) -> Result<(), TransferError> {
    let key = origin.key(package, room)?;
    let _writer = writer(store, &key)?;
    let mut doc = decode(store, &key)?;
    if is_retired(&doc) {
        return Ok(());
    }
    origin.validate(&doc, room)?;
    stamp_retired(&mut doc, why)?;
    persist(store, &key, &mut doc)
}

/// The origin ids a transferred record came from, oldest first.
pub fn provenance(doc: &AutoCommit) -> Vec<RecordId> {
    let Some(meta) = meta_ro(doc) else {
        return Vec::new();
    };
    let ObjectAt::Object(list) = object_at(doc, &meta, FORMERLY, ObjType::List) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(id) => RecordId::parse(id),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{RecordOwners, RecordStore, room_scope};
    use automerge::ROOT;

    const PACKAGE: &str = "so.tilo.transfer-test";
    const FROM: &str = "3f2b1c00-0000-4000-8000-00000000000a";
    const TO: &str = "3f2b1c00-0000-4000-8000-00000000000b";

    fn store(root: &std::path::Path) -> Rc<dyn DurableStore> {
        Rc::new(app_state::FsStore::new(root.to_path_buf()))
    }

    fn body(doc: &AutoCommit) -> Option<String> {
        match doc.get(ROOT, "body") {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    fn moved() -> Retirement {
        Retirement::Moved(Some(Successor {
            room: TO.into(),
            record: "uuid:successor".into(),
            by: "person:ada".into(),
        }))
    }

    #[test]
    fn a_retirement_travels_as_one_change_and_is_read_back_whole() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", "draft").unwrap();
        doc.commit();
        let before = doc.get_heads();
        stamp_retired(&mut doc, &moved()).unwrap();
        assert_eq!(retirement(&doc), Some(moved()));
        assert_eq!(doc.get_changes(&before).len(), 1);
        let mut peer = doc.fork_at(&before).unwrap();
        assert!(!is_retired(&peer));
        peer.merge_incoming(&mut doc).unwrap();
        assert_eq!(
            retirement(&peer),
            Some(moved()),
            "a peer holding it learns it moved"
        );
        stamp_retired(&mut doc, &Retirement::Abandoned).unwrap();
        assert_eq!(
            retirement(&doc),
            Some(moved()),
            "the first retirement stands"
        );
    }

    fn seeded(root: &std::path::Path) -> (RecordCheckout, RecordStore) {
        let checkout = RecordCheckout::new(
            RecordId::derived("default", &RecordId::for_tab(0)),
            RecordKind::new("transfer.test"),
        );
        let records = RecordOwners::new(root.to_path_buf())
            .open(PACKAGE, FROM)
            .unwrap();
        records.create_record(&checkout).unwrap();
        records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "first draft").unwrap();
            })
            .unwrap();
        records.try_flush().unwrap();
        (checkout, records)
    }

    #[test]
    fn a_moved_record_keeps_its_history_and_later_edits_and_leaves_its_room() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, old_writer) = seeded(temp.path());
        let origin = Origin::Record(checkout.clone());

        // The old app still holds the record: reading it needs no writer.
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let known = snapshot.clone().get_heads();
        let target = RecordId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let installed = install(&store, PACKAGE, TO, &mut arrived).unwrap();
        assert_eq!(installed.id, target);
        assert_eq!(provenance(&arrived), std::slice::from_ref(&checkout.id));
        assert!(
            arrived.get_change_by_hash(&known[0]).is_some(),
            "the source history travels"
        );

        // It edits once more after the read, then exits.
        old_writer
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "first draft, last words").unwrap();
            })
            .unwrap();
        old_writer.try_flush().unwrap();
        assert!(matches!(
            retire(&store, PACKAGE, FROM, &origin, &moved()),
            Err(TransferError::Busy)
        ));
        drop(old_writer);

        let last = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert_eq!(changes_since(&last, &known), 1);
        assert_eq!(catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(), 1);
        assert_eq!(catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(), 0);
        retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();

        let destination = read(&store, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("first draft, last words")
        );
        assert_eq!(room_scope(&destination).as_deref(), Some(TO));
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(RecordOpenError::Retired))
        ));
        let source_room = RecordOwners::new(temp.path().to_path_buf())
            .open(PACKAGE, FROM)
            .unwrap();
        assert!(matches!(
            source_room.try_open_record(&checkout),
            Err(RecordOpenError::Retired)
        ));
        let retired = decode(&store, &key(PACKAGE, FROM, &checkout.id).unwrap()).unwrap();
        assert_eq!(retirement(&retired), Some(moved()));
        assert_eq!(
            body(&retired).as_deref(),
            Some("first draft, last words"),
            "the retirement is one more change, not an erasure"
        );
    }

    #[test]
    fn a_document_joins_another_room_only_as_exactly_the_minted_record_it_claims() {
        let dir = tempfile::tempdir().unwrap();
        let (checkout, writer) = seeded(dir.path());
        drop(writer);
        let source = read(
            &store(dir.path()),
            PACKAGE,
            FROM,
            &Origin::Record(checkout.clone()),
        )
        .unwrap();
        let target = RecordId::minted();
        let arriving = |doc: &AutoCommit| doc.clone().save();
        let mut rehomed = rehome(
            &source,
            &Origin::Record(checkout.clone()),
            FROM,
            TO,
            &target,
        )
        .unwrap();
        let bytes = arriving(&rehomed);
        let (kept, identity) = validate_import(&bytes, TO, &target, bytes.len()).unwrap();
        assert_eq!(identity.id, target);
        assert_eq!(body(&kept).as_deref(), Some("first draft"));
        assert_eq!(provenance(&kept), std::slice::from_ref(&checkout.id));

        assert!(
            validate_import(&bytes, TO, &target, bytes.len() - 1).is_err(),
            "size is bounded"
        );
        assert!(
            validate_import(&bytes, FROM, &target, bytes.len()).is_err(),
            "scoped to the room it joins"
        );
        assert!(
            validate_import(&bytes, TO, &RecordId::minted(), bytes.len()).is_err(),
            "it is only the record it claims"
        );
        let derived = RecordId::derived("notes", &target);
        let under_derived = rehome(
            &source,
            &Origin::Record(checkout.clone()),
            FROM,
            TO,
            &derived,
        )
        .unwrap();
        let derived_bytes = arriving(&under_derived);
        assert!(
            validate_import(&derived_bytes, TO, &derived, derived_bytes.len()).is_err(),
            "only a freshly minted id joins a room"
        );

        let mut referring = rehomed.clone();
        assert!(crate::record::add_reference(
            &mut referring,
            &crate::record::RecordRef {
                target: RecordId::minted(),
                relation: "cites".into(),
            }
        ));
        let referring = arriving(&referring);
        assert!(
            validate_import(&referring, TO, &target, referring.len()).is_err(),
            "no identities of other records ride along"
        );
        let mut indexing = rehomed.clone();
        indexing
            .put_object(ROOT, crate::record::INDEX, ObjType::Map)
            .unwrap();
        let indexing = arriving(&indexing);
        assert!(validate_import(&indexing, TO, &target, indexing.len()).is_err());
        let meta = meta_obj(&mut rehomed).unwrap();
        rehomed.put(&meta, "alias", "uuid:other").unwrap();
        let aliased = arriving(&rehomed);
        assert!(validate_import(&aliased, TO, &target, aliased.len()).is_err());
        let mut retired = read(
            &store(dir.path()),
            PACKAGE,
            FROM,
            &Origin::Record(checkout.clone()),
        )
        .unwrap();
        let mut retired = rehome(
            &retired.fork(),
            &Origin::Record(checkout),
            FROM,
            TO,
            &target,
        )
        .unwrap();
        stamp_retired(&mut retired, &moved()).unwrap();
        let retired = arriving(&retired);
        assert!(validate_import(&retired, TO, &target, retired.len()).is_err());
    }

    #[test]
    fn a_record_from_another_device_is_kept_only_as_the_live_record_it_claims_to_be() {
        let holder_dir = tempfile::tempdir().unwrap();
        let holder = store(holder_dir.path());
        let (checkout, writer) = seeded(holder_dir.path());
        drop(writer);
        let bytes = read_by_id(&holder, PACKAGE, FROM, &checkout.id)
            .unwrap()
            .save();

        let here_dir = tempfile::tempdir().unwrap();
        let here = store(here_dir.path());
        assert!(
            adopt_fetched(&here, PACKAGE, FROM, &RecordId::minted(), &bytes).is_err(),
            "bytes offered as another record are refused"
        );
        assert!(
            adopt_fetched(&here, PACKAGE, TO, &checkout.id, &bytes).is_err(),
            "a record of one room is never kept as another room's"
        );
        assert!(adopt_fetched(&here, PACKAGE, FROM, &checkout.id, b"not a document").is_err());
        let kept = adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes).unwrap();
        assert_eq!(kept, checkout);
        assert_eq!(
            adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes).unwrap(),
            checkout,
            "holding it already merges rather than refuses"
        );
        let doc = read_by_id(&here, PACKAGE, FROM, &checkout.id).unwrap();
        assert_eq!(body(&doc).as_deref(), Some("first draft"));

        let mut retired = doc.clone();
        stamp_retired(&mut retired, &moved()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(
            adopt_fetched(
                &store(elsewhere.path()),
                PACKAGE,
                FROM,
                &checkout.id,
                &retired.save()
            )
            .is_err(),
            "a record that left its room is never brought back to life"
        );
    }

    #[test]
    fn a_transfer_never_overwrites_or_crosses_into_its_own_room() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, writer) = seeded(temp.path());
        drop(writer);
        let origin = Origin::Record(checkout.clone());
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert!(matches!(
            rehome(&snapshot, &origin, FROM, FROM, &RecordId::minted()),
            Err(TransferError::NotTransferable(_))
        ));
        assert!(
            rehome(&snapshot, &origin, TO, FROM, &RecordId::minted()).is_err(),
            "a record is only read from the room it is scoped to"
        );
        let target = RecordId::minted();
        let mut first = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        install(&store, PACKAGE, TO, &mut first).unwrap();
        let mut again = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        assert!(matches!(
            install(&store, PACKAGE, TO, &mut again),
            Err(TransferError::Open(RecordOpenError::AlreadyExists))
        ));
        let foreign = Origin::Record(RecordCheckout::new(target, checkout.kind.clone()));
        assert!(
            read(&store, PACKAGE, FROM, &foreign).is_err(),
            "a destination id means nothing in the source room"
        );
    }

    #[test]
    fn an_app_document_arrives_as_a_record_and_its_room_keeps_a_retired_one() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut app =
            app_state::DocStore::shared(Rc::clone(&store), StorageKey::package(PACKAGE)).unwrap();
        let mut doc = app.load_or_new();
        doc.put(ROOT, "body", "kept in the app document").unwrap();
        app.save(&mut doc).unwrap();

        let origin = Origin::AppDocument(RecordKind::new("tilo.app-document"));
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let target = RecordId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let installed = install(&store, PACKAGE, TO, &mut arrived).unwrap();
        assert_eq!(installed.kind.as_str(), "tilo.app-document");
        let destination = read(&store, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("kept in the app document")
        );
        assert_eq!(crate::record::home(&destination).as_deref(), Some(FROM));

        assert!(
            matches!(
                retire(&store, PACKAGE, FROM, &origin, &moved()),
                Err(TransferError::Busy)
            ),
            "not while the app still writes it"
        );
        drop(app);
        retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(RecordOpenError::Retired))
        ));
    }
}
