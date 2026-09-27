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
//! Every function works on the durable store a room's app profile keeps,
//! through the same keys, leases and validation the record owners use. A
//! write needs the key's exclusive lease, so it fails with
//! [`TransferError::Busy`] while an app process still has the record open;
//! a caller waits for that process to go rather than racing its writer.

use std::rc::Rc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, ScalarValue, Value};
use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;

use super::{
    FORMERLY, ObjectAt, RecordCheckout, RecordId, RecordOpenError, StringAt, identity, meta_obj,
    meta_ro, object_at, room_scope, string_at, validate_identity,
};

/// Key, in a record's meta map, marking a record that left its room.
pub(super) const RETIRED: &str = "retired";

/// Why a record is retired, as stamped into its last change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// Moved to another room; the successor there carries its history.
    Moved,
    /// Imported for a transfer that did not complete; nothing ever opened it.
    Abandoned,
}

impl Retirement {
    fn as_str(self) -> &'static str {
        match self {
            Self::Moved => "moved",
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
    match string_at(doc, &meta, RETIRED) {
        StringAt::String(reason) if reason == Retirement::Abandoned.as_str() => {
            Some(Retirement::Abandoned)
        }
        StringAt::String(_) | StringAt::Invalid => Some(Retirement::Moved),
        StringAt::Missing => None,
    }
}

/// Whether `doc` is a record that left its room.
pub fn is_retired(doc: &AutoCommit) -> bool {
    retirement(doc).is_some()
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

/// Read `checkout` as `package` last made it durable in `room`, without
/// taking its writer: the app showing it may still be running.
///
/// # Errors
/// A missing, retired, foreign-room or malformed record, or storage failure.
pub fn read(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    checkout: &RecordCheckout,
) -> Result<AutoCommit, TransferError> {
    let doc = decode(store, &key(package, room, &checkout.id)?)?;
    validate_identity(&doc, checkout, room)?;
    Ok(doc)
}

/// The destination's copy of `source`: its whole history, one more change
/// giving it `target`'s id and `to_room`'s scope, and the origin id kept
/// as provenance. `source` is not changed.
///
/// # Errors
/// A source that is not `origin`'s live record, or a transfer into the room
/// it is already in.
pub fn rehome(
    source: &AutoCommit,
    origin: &RecordCheckout,
    to_room: &str,
    target: &RecordId,
) -> Result<AutoCommit, TransferError> {
    if origin.id.is_package_primary() || target.is_package_primary() {
        return Err(TransferError::NotTransferable(
            "a room's own package document never leaves its room",
        ));
    }
    if RecordId::parse(target.as_str()).as_ref() != Some(target) || target == &origin.id {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    let from_room = room_scope(source).ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(source, origin, &from_room)?;
    if from_room == to_room || to_room.is_empty() {
        return Err(TransferError::NotTransferable(
            "the record is already in that room",
        ));
    }
    // A fork continues the same change graph under a writer of its own.
    let mut doc = source.clone().fork();
    let meta = meta_obj(&mut doc).ok_or(RecordOpenError::IdentityMismatch)?;
    let put = |doc: &mut AutoCommit, key: &str, value: &str| {
        doc.put(&meta, key, value)
            .map_err(|error| TransferError::Automerge(error.to_string()))
    };
    put(&mut doc, "id", target.as_str())?;
    put(&mut doc, super::ROOM, to_room)?;
    let formerly = match object_at(&doc, &meta, FORMERLY, ObjType::List) {
        ObjectAt::Object(list) => list,
        ObjectAt::Missing => doc
            .put_object(&meta, FORMERLY, ObjType::List)
            .map_err(|error| TransferError::Automerge(error.to_string()))?,
        ObjectAt::Invalid => return Err(RecordOpenError::IdentityMismatch.into()),
    };
    let len = doc.length(&formerly);
    doc.insert(&formerly, len, origin.id.as_str())
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    doc.commit_with(CommitOptions::default().with_message("tilo.record.transfer".to_owned()));
    let arrived = RecordCheckout::new(target.clone(), origin.kind.clone());
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

/// Whether no app process holds `checkout`'s writer in `room` any more: the
/// point after which nothing more can arrive from it.
///
/// # Errors
/// An invalid key or storage failure.
pub fn writer_released(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    checkout: &RecordCheckout,
) -> Result<bool, TransferError> {
    match writer(store, &key(package, room, &checkout.id)?) {
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

/// Stamp `checkout` in `room` as retired: one last change in its own
/// history, after which it is never opened as a live record again.
///
/// # Errors
/// [`TransferError::Busy`] while an app still holds the record, a missing
/// or foreign record, or storage failure.
pub fn retire(
    store: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
    checkout: &RecordCheckout,
    why: Retirement,
) -> Result<(), TransferError> {
    let key = key(package, room, &checkout.id)?;
    let _writer = writer(store, &key)?;
    let mut doc = decode(store, &key)?;
    if is_retired(&doc) {
        return Ok(());
    }
    validate_identity(&doc, checkout, room)?;
    let meta = meta_obj(&mut doc).ok_or(RecordOpenError::IdentityMismatch)?;
    doc.put(&meta, RETIRED, why.as_str())
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    doc.commit_with(CommitOptions::default().with_message(match why {
        Retirement::Moved => "tilo.record.moved-out".to_owned(),
        Retirement::Abandoned => "tilo.record.transfer-abandoned".to_owned(),
    }));
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

    fn seeded(root: &std::path::Path) -> (RecordCheckout, super::super::RecordStore) {
        let checkout = RecordCheckout::new(
            RecordId::derived("default", &RecordId::for_tab(0)),
            super::super::RecordKind::new("transfer.test"),
        );
        let mut owners = super::super::RecordOwners::new(root.to_path_buf());
        let records = owners.open(PACKAGE, FROM).unwrap();
        records.create_record(&checkout).unwrap();
        records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "first draft").unwrap()
            })
            .unwrap();
        records.try_flush().unwrap();
        (checkout, records)
    }

    #[test]
    fn a_moved_record_keeps_its_history_and_later_edits_and_leaves_its_room() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (origin, old_writer) = seeded(temp.path());

        // The old app still holds the record: reading it needs no writer.
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let known = snapshot.clone().get_heads();
        let target = RecordId::minted();
        let mut arrived = rehome(&snapshot, &origin, TO, &target).unwrap();
        let installed = install(&store, PACKAGE, TO, &mut arrived).unwrap();
        assert_eq!(installed.id, target);
        assert_eq!(provenance(&arrived), std::slice::from_ref(&origin.id));
        assert!(
            arrived.get_change_by_hash(&known[0]).is_some(),
            "the source history travels"
        );

        // It edits once more after the read, then exits.
        old_writer
            .with(&origin.id, |doc| {
                doc.put(ROOT, "body", "first draft, last words").unwrap();
            })
            .unwrap();
        old_writer.try_flush().unwrap();
        assert!(matches!(
            retire(&store, PACKAGE, FROM, &origin, Retirement::Moved),
            Err(TransferError::Busy)
        ));
        drop(old_writer);

        let last = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert_eq!(changes_since(&last, &known), 1);
        assert_eq!(catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(), 1);
        assert_eq!(catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(), 0);
        retire(&store, PACKAGE, FROM, &origin, Retirement::Moved).unwrap();

        let destination = read(&store, PACKAGE, TO, &installed).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("first draft, last words")
        );
        assert_eq!(room_scope(&destination).as_deref(), Some(TO));
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(RecordOpenError::Retired))
        ));
        let mut owners = super::super::RecordOwners::new(temp.path().to_path_buf());
        let source_room = owners.open(PACKAGE, FROM).unwrap();
        assert!(matches!(
            source_room.try_open_record(&origin),
            Err(RecordOpenError::Retired)
        ));
        let retired = decode(&store, &key(PACKAGE, FROM, &origin.id).unwrap()).unwrap();
        assert_eq!(retirement(&retired), Some(Retirement::Moved));
        assert_eq!(
            body(&retired).as_deref(),
            Some("first draft, last words"),
            "the retirement is one more change, not an erasure"
        );
    }

    #[test]
    fn a_transfer_never_overwrites_or_crosses_into_its_own_room() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (origin, writer) = seeded(temp.path());
        drop(writer);
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert!(matches!(
            rehome(&snapshot, &origin, FROM, &RecordId::minted()),
            Err(TransferError::NotTransferable(_))
        ));
        let target = RecordId::minted();
        let mut first = rehome(&snapshot, &origin, TO, &target).unwrap();
        install(&store, PACKAGE, TO, &mut first).unwrap();
        let mut again = rehome(&snapshot, &origin, TO, &target).unwrap();
        assert!(matches!(
            install(&store, PACKAGE, TO, &mut again),
            Err(TransferError::Open(RecordOpenError::AlreadyExists))
        ));
        let foreign = RecordCheckout::new(target, origin.kind.clone());
        assert!(
            read(&store, PACKAGE, FROM, &foreign).is_err(),
            "a destination id means nothing in the source room"
        );
    }
}
