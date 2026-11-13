//! Transfer: the explicit verb that takes a record across a workspace boundary.
//!
//! A record id names a document only inside the workspace whose replica hydrates it
//! ([`super::workspace_scope`]), so nothing crosses workspaces by reference. Transfer is the
//! counterpart of [`super::index::duplicate`] for a document that already has content: the
//! destination workspace receives the record's whole change graph under a fresh id of its own,
//! scoped to that workspace, with the source id kept in its `formerly` list as provenance. Nothing
//! ever looks a record up through that list.
//!
//! A record's workspace is stored under `room`, the key every record is written with, so a document
//! this module writes names it the way every reader knows; a document written while the field was
//! named `workspace` holds [`super::WORKSPACE_ALIAS`] and is read where it is.
//!
//! Moving is transfer followed by [`retire`]: the source record gets one last change saying it
//! left, which stays in its history the way a deletion does, and it is never opened as a live
//! document again ([`super::RecordOpenError::Retired`]). Copying is transfer alone: the source
//! keeps going and the two histories diverge from here. An app that keeps one document for itself
//! in a workspace (its record index) transfers the same way ([`Origin::AppDocument`]).
//!
//! Every function works on the durable store a workspace's app profile keeps, through the same
//! keys, leases and validation the record owners use. A write needs the key's exclusive lease, so
//! it fails with [`TransferError::Busy`] while an app process still has the record open; a caller
//! waits for that process to go rather than racing its writer.
//!
//! A write returns a [`Landing`] rather than waiting for the disk: the shell's frame loop drives
//! transfers and never waits for a commit. The next process to open the record reads it from disk,
//! so whatever hands the record over asks [`Landing::durability`] until the write has landed, while
//! the store's own thread does the waiting.

use std::rc::Rc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, ScalarValue, Value};
pub use durable_store::{Durability, DurableStore};
use durable_store::{Revision, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;

use super::{
    FORMERLY, ObjectAt, RecordCheckout, RecordId, RecordKind, RecordOpenError, identity, meta_obj,
    meta_ro, object_at, validate_identity,
};

/// The kind an app's own document ([`Origin::AppDocument`]) takes when it
/// arrives in another workspace as a record.
pub const APP_DOCUMENT_KIND: &str = "tilo.app-document";

pub(super) use ::collaboration::record::retirement::RETIRED;
pub use ::collaboration::record::retirement::{Retirement, Successor, is_retired, retirement};

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

impl TransferError {
    /// Whether this is only a not-yet: on a thread that must not wait on the
    /// disk ([`durable_store::waiting`]) the store's own thread is still
    /// opening or reading the document, and asking again later goes on.
    #[must_use]
    pub fn is_opening(&self) -> bool {
        matches!(
            self,
            Self::Store(app_state::StoreError::Opening(_)) | Self::Open(RecordOpenError::Opening)
        )
    }
}

impl From<durable_store::StoreError> for TransferError {
    fn from(error: durable_store::StoreError) -> Self {
        match error {
            durable_store::StoreError::Busy(_) => Self::Busy,
            other => Self::Store(other.into()),
        }
    }
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
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    Ok(())
}

/// What a transfer takes out of the source workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Record(RecordCheckout),
    /// The document an app keeps for itself in a workspace, its record index.
    /// It has no record identity of its own there, and arrives in the
    /// destination as a record of this kind.
    AppDocument(RecordKind),
}

impl Origin {
    fn key(&self, package: &str, workspace: &str) -> Result<StorageKey, TransferError> {
        match self {
            Self::Record(checkout) => key(package, workspace, &checkout.id),
            Self::AppDocument(_) => {
                app_state::validate_document_namespace(package, workspace)?;
                Ok(StorageKey::app(package))
            }
        }
    }

    /// Refuse anything but this origin's live document in `workspace`.
    pub fn validate(&self, doc: &AutoCommit, workspace: &str) -> Result<(), TransferError> {
        match self {
            Self::Record(checkout) => Ok(validate_identity(doc, checkout, workspace)?),
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

fn key(package: &str, workspace: &str, id: &RecordId) -> Result<StorageKey, TransferError> {
    if id.is_package_primary() {
        return Err(TransferError::NotTransferable(
            "a workspace's own package document never leaves its workspace",
        ));
    }
    let record = id.storage_key();
    app_state::validate_document_identity(package, workspace, &record)?;
    Ok(StorageKey::document(package, workspace, record))
}

fn decode(store: &Rc<dyn DurableStore>, key: &StorageKey) -> Result<AutoCommit, TransferError> {
    decode_stored(store, key)?.ok_or_else(|| RecordOpenError::MissingRecord.into())
}

/// `key`'s document, `None` when nothing is stored there. One read: a store
/// that prepares reads for a thread that must not wait hands each one over
/// once.
fn decode_stored(
    store: &Rc<dyn DurableStore>,
    key: &StorageKey,
) -> Result<Option<AutoCommit>, TransferError> {
    let Some(stored) = store.read(key)?.filter(|stored| !stored.bytes.is_empty()) else {
        return Ok(None);
    };
    let label = store
        .local_path(key)
        .unwrap_or_else(|| std::path::PathBuf::from(key.to_string()));
    let (doc, _) = app_state::decode_canonical(&stored.bytes, false, &label)?;
    Ok(Some(doc))
}

/// Read `origin` as `package` last made it durable in `workspace`, without
/// taking its writer: the app showing it may still be running.
///
/// # Errors
/// A missing, retired, foreign-workspace or malformed document, or storage failure.
pub fn read(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    origin: &Origin,
) -> Result<AutoCommit, TransferError> {
    let doc = decode(store, &origin.key(package, workspace)?)?;
    origin.validate(&doc, workspace)?;
    Ok(doc)
}

/// A live record of `package` in `workspace` by its id alone, whatever its kind:
/// what another device asks for when all it knows is the id.
///
/// # Errors
/// A missing, retired, foreign-workspace or malformed record, or storage failure.
pub fn read_by_id(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    id: &RecordId,
) -> Result<AutoCommit, TransferError> {
    let doc = decode(store, &key(package, workspace, id)?)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, workspace)?;
    Ok(doc)
}

/// Verify `bytes` another device sent as record `id` of `workspace`, and keep it
/// here: installed when new, merged when this device already holds it. The
/// bytes are only trusted as far as they prove to be that live record.
///
/// # Errors
/// Bytes that are not a live record `id` scoped to `workspace`, or storage failure.
pub fn adopt_fetched(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    id: &RecordId,
    bytes: &[u8],
) -> Result<(RecordCheckout, Option<Landing>), TransferError> {
    let label = std::path::PathBuf::from(format!("fetched {id}"));
    let (mut doc, _) = app_state::decode_canonical(bytes, false, &label)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, workspace)?;
    // One writer and one read for both cases: a thread that must not wait
    // is answered Opening until the store's thread has leased the key, and
    // a second lease of it would start that over.
    let key = key(package, workspace, &checkout.id)?;
    let _writer = writer(store, &key)?;
    let landing = match decode_stored(store, &key)? {
        None => Some(persist(store, &key, &mut doc)?),
        Some(held) => merge_into(store, &key, held, &checkout, workspace, &doc)?.1,
    };
    Ok((checkout, landing))
}

/// Identity keys a record's meta map may hold; anything else could claim
/// more than the one record the document is. A record carries its workspace
/// under the key every record is written with, or under
/// [`super::WORKSPACE_ALIAS`], the key one build wrote it under; both import
/// as they are.
const IMPORTABLE_META: [&str; 8] = [
    "id",
    "kind",
    "home",
    super::WORKSPACE,
    super::WORKSPACE_ALIAS,
    FORMERLY,
    "refs",
    "blobs",
];

/// Check `bytes` offered to become record `id` of `workspace` with their whole
/// history, before anything keeps them: at most `max_bytes`, a complete
/// document whose identity is exactly that freshly minted record scoped to
/// that workspace, live, holding no workspace index and no references to other
/// records, and naming earlier ids only as provenance.
///
/// # Errors
/// Bytes that are too large, malformed, or not exactly that record.
pub fn validate_import(
    bytes: &[u8],
    workspace: &str,
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
            "a document joins a workspace only as a newly minted record",
        ));
    }
    let label = std::path::PathBuf::from(format!("imported {id}"));
    let (doc, _) = app_state::decode_canonical(bytes, false, &label)?;
    let checkout = identity(&doc)
        .filter(|found| &found.id == id)
        .ok_or(RecordOpenError::IdentityMismatch)?;
    validate_identity(&doc, &checkout, workspace)?;
    if super::workspace_scope(&doc).as_deref() != Some(workspace) {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    if doc
        .get(automerge::ROOT, super::INDEX)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(TransferError::NotTransferable(
            "a document listing other records cannot join a workspace",
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
            "a document referring to other records cannot join a workspace",
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
    workspace: &str,
    origin: &Origin,
    heads: &[ChangeHash],
) -> Result<AutoCommit, TransferError> {
    let mut stored = decode(store, &origin.key(package, workspace)?)?;
    let doc = stored
        .fork_at(heads)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    origin.validate(&doc, workspace)?;
    Ok(doc)
}

/// The destination's copy of `source`, read from `origin` in `from_workspace`:
/// its whole history, one more change giving it `target`'s id and
/// `to_workspace`'s scope, and a record's origin id kept as provenance. `source`
/// is not changed.
pub fn rehome(
    source: &AutoCommit,
    origin: &Origin,
    from_workspace: &str,
    to_workspace: &str,
    target: &RecordId,
) -> Result<AutoCommit, TransferError> {
    if target.is_package_primary()
        || matches!(origin, Origin::Record(checkout) if checkout.id.is_package_primary())
    {
        return Err(TransferError::NotTransferable(
            "a workspace's own package document never leaves its workspace",
        ));
    }
    if RecordId::parse(target.as_str()).as_ref() != Some(target)
        || matches!(origin, Origin::Record(checkout) if &checkout.id == target)
    {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    origin.validate(source, from_workspace)?;
    if from_workspace == to_workspace || to_workspace.is_empty() || from_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "the record is already in that workspace",
        ));
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    // A fork continues the same change graph under a writer of its own.
    let mut doc = source.clone().fork();
    match origin {
        Origin::Record(checkout) => {
            let meta = meta_obj(&mut doc).ok_or(RecordOpenError::IdentityMismatch)?;
            doc.put(&meta, "id", target.as_str()).map_err(automerge)?;
            doc.put(&meta, super::WORKSPACE, to_workspace)
                .map_err(automerge)?;
            // The fork names its workspace under the one key every reader
            // knows: a document written while the field was named
            // `workspace` would otherwise name a second one.
            let _ = doc.delete(&meta, super::WORKSPACE_ALIAS);
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
        // was minted in the source workspace and is now scoped to the destination.
        Origin::AppDocument(kind) => {
            if !super::init_scoped(&mut doc, target, kind, from_workspace, Some(to_workspace)) {
                return Err(RecordOpenError::IdentityMismatch.into());
            }
        }
    }
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    let arrived = RecordCheckout::new(target.clone(), origin.kind().clone());
    validate_identity(&doc, &arrived, to_workspace)?;
    Ok(doc)
}

/// A branch's copy of `source`, a document of a workspace branched into `to_workspace`: its whole
/// history, under a writer of its own, with one more change naming `to_workspace` as the workspace
/// it is in when it names one at all. Its id stays: a record id names a document only inside its
/// workspace ([`super::workspace_scope`]), and a branch is a whole workspace forked, so every
/// reference one of its records makes to another still names the same one. Where it was minted
/// (`home`) stays too, as its provenance, but for a record any app makes from its kind alone,
/// which is minted again in the branch ([`minted_again`]). `source` is not changed.
///
/// # Errors
/// `to_workspace` is empty, or the document's metadata cannot name it.
pub fn branch(source: &AutoCommit, to_workspace: &str) -> Result<AutoCommit, TransferError> {
    if to_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "a branch is made into a workspace",
        ));
    }
    // A fork continues the same change graph under a writer of its own, so a change made in the
    // branch never takes a number a change in the source already has.
    let mut doc = match minted_again(source, to_workspace)? {
        Some(mut minted) => minted.fork(),
        None => source.clone().fork(),
    };
    let Some(scoped) = super::workspace_scope(&doc) else {
        return Ok(doc);
    };
    if scoped == to_workspace {
        return Ok(doc);
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    let meta = meta_obj(&mut doc).ok_or(RecordOpenError::IdentityMismatch)?;
    doc.put(&meta, super::WORKSPACE, to_workspace)
        .map_err(automerge)?;
    let _ = doc.delete(&meta, super::WORKSPACE_ALIAS);
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    Ok(doc)
}

/// `source` minted again in `to_workspace`, when it is a record any app makes from its kind alone
/// where it holds none ([`collab_types::workspace_record_id`]: the workspace's emoji, its agents)
/// and its genesis made it: its whole history, but with the genesis an app of the branch makes
/// ([`super::genesis`]) as its first change, and every change after it built on that one. An app of
/// the branch that opens the record before the branch's copy reached it makes the same genesis, so
/// the two meet as one record. `None` for any other document, which keeps where it was minted.
fn minted_again(
    source: &AutoCommit,
    to_workspace: &str,
) -> Result<Option<AutoCommit>, TransferError> {
    use automerge::legacy::{ElementId, Key, ObjectId, OpId, OpType};

    let Some(checkout) = identity(source) else {
        return Ok(None);
    };
    if checkout.id.as_str() != collab_types::workspace_record_id(checkout.kind.as_str()) {
        return Ok(None);
    }
    let Some(home) = super::home(source).filter(|home| home != to_workspace) else {
        return Ok(None);
    };
    let from = super::genesis_actor_of(&checkout.id, &checkout.kind, &home);
    let to = super::genesis_actor_of(&checkout.id, &checkout.kind, to_workspace);
    let changes = source.clone().get_changes(&[]);
    // Its genesis made it: the genesis actor's one change comes first and depends on nothing.
    let made_by_genesis = changes
        .first()
        .is_some_and(|first| first.actor_id() == &from && first.deps().is_empty())
        && changes
            .iter()
            .filter(|change| change.actor_id() == &from)
            .count()
            == 1;
    if !made_by_genesis {
        return Ok(None);
    }
    let rename = |id: &mut OpId| {
        if id.1 == from {
            id.1 = to.clone();
        }
    };
    let mut hashes = std::collections::HashMap::new();
    let mut minted = Vec::with_capacity(changes.len());
    for change in changes {
        let was = change.hash();
        let mut expanded = change.decode();
        expanded.hash = None;
        expanded.deps = expanded
            .deps
            .iter()
            .map(|dep| hashes.get(dep).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| TransferError::Automerge(format!("change {was} before its base")))?;
        let genesis = expanded.actor_id == from;
        if genesis {
            expanded.actor_id = to.clone();
        }
        for op in &mut expanded.operations {
            if let ObjectId::Id(id) = &mut op.obj {
                rename(id);
            }
            if let Key::Seq(ElementId::Id(id)) = &mut op.key {
                rename(id);
            }
            op.pred = op
                .pred
                .iter()
                .cloned()
                .map(|mut pred| {
                    rename(&mut pred);
                    pred
                })
                .collect();
            // The genesis names the workspace that minted it, and the one it is in.
            let names_home = match (&op.key, &op.action) {
                (Key::Map(key), OpType::Put(ScalarValue::Str(named))) => {
                    genesis
                        && ["home", super::WORKSPACE].contains(&key.as_str())
                        && named.as_str() == home
                }
                _ => false,
            };
            if names_home {
                op.action = OpType::Put(ScalarValue::from(to_workspace));
            }
        }
        let change = automerge::Change::from(expanded);
        hashes.insert(was, change.hash());
        minted.push(change);
    }
    let mut doc = AutoCommit::new();
    doc.apply_incoming(minted)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    if super::genesis_actor(&doc).as_ref() != Some(&to) {
        return Err(RecordOpenError::IdentityMismatch.into());
    }
    validate_identity(&doc, &checkout, to_workspace)?;
    Ok(Some(doc))
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

/// Whether no app process holds `origin`'s writer in `workspace` any more: the
/// point after which nothing more can arrive from it.
///
/// # Errors
/// An invalid key or storage failure.
pub fn writer_released(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    origin: &Origin,
) -> Result<bool, TransferError> {
    match writer(store, &origin.key(package, workspace)?) {
        Ok(_) => Ok(true),
        Err(TransferError::Busy) => Ok(false),
        Err(error) => Err(error),
    }
}

/// A transfer's write, accepted by the store and on its way to disk.
///
/// Another process reads the record next (the instance that opens it, an
/// app restarted around a merge), so the hand-off to it waits until the
/// write is [`Durability::Durable`]. Asking never blocks: on a thread that
/// may wait the write had landed before it was returned, and on one that
/// must not ([`durable_store::waiting`]) the store's thread commits it while
/// the document stays leased to it. Nobody else writes the record before
/// it lands.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use = "hand the record over only once its write has landed"]
pub struct Landing {
    key: StorageKey,
    revision: Revision,
}

impl Landing {
    pub(crate) fn new(key: StorageKey, revision: Revision) -> Self {
        Self { key, revision }
    }

    /// Whether the write has reached the disk, in `store`, the store that
    /// accepted it. Never blocks.
    pub fn durability(&self, store: &dyn DurableStore) -> Durability {
        store.durability(&self.key, self.revision)
    }

    /// How all of `landings` in `store` stand: failed if any failed,
    /// pending while any is, durable once every one is.
    pub fn all(store: &dyn DurableStore, landings: &[Self]) -> Durability {
        let mut all = Durability::Durable;
        for landing in landings {
            match landing.durability(store) {
                Durability::Durable => {}
                Durability::Pending => all = Durability::Pending,
                failed @ Durability::Failed(_) => return failed,
            }
        }
        all
    }
}

/// Start `key` committing as `doc`, without waiting for it where this
/// thread must not.
fn persist(
    store: &Rc<dyn DurableStore>,
    key: &StorageKey,
    doc: &mut AutoCommit,
) -> Result<Landing, TransferError> {
    let revision = store.write(key, WriteOp::Replace(doc.save()), None)?;
    match store.flush_where_allowed(key) {
        Ok(()) | Err(durable_store::StoreError::Pending(_)) => {
            Ok(Landing::new(key.clone(), revision))
        }
        Err(error) => Err(error.into()),
    }
}

/// Make `doc` (from [`rehome`]) a record of `package` in `workspace`. Refuses to
/// overwrite anything already stored under its id. Open it elsewhere once
/// the [`Landing`] has landed.
///
/// # Errors
/// A document not scoped to `workspace`, an occupied id, or storage failure.
pub fn install(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    doc: &mut AutoCommit,
) -> Result<(RecordCheckout, Landing), TransferError> {
    let checkout = identity(doc).ok_or(RecordOpenError::IdentityMismatch)?;
    install_as(store, package, workspace, checkout, doc)
}

/// [`install`] a document that arrived for `checkout`, including the workspace's
/// package-primary record, whose identity the document does not carry.
pub fn install_as(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    checkout: RecordCheckout,
    doc: &mut AutoCommit,
) -> Result<(RecordCheckout, Landing), TransferError> {
    validate_identity(doc, &checkout, workspace)?;
    let key = key(package, workspace, &checkout.id)?;
    let _writer = writer(store, &key)?;
    if store
        .read(&key)?
        .is_some_and(|stored| !stored.bytes.is_empty())
    {
        return Err(RecordOpenError::AlreadyExists.into());
    }
    let landing = persist(store, &key, doc)?;
    Ok((checkout, landing))
}

/// Put `doc` in place as the workspace's own document of `package` in `workspace`
/// (the [package-primary](RecordId::package_primary) record), as an earlier
/// version of the app left it on this device: how a workspace written before a
/// change of the app's storage is opened in a test. Refuses to overwrite
/// one.
///
/// # Errors
/// An invalid package or workspace, an occupied workspace document, or storage
/// failure.
pub fn install_workspace_document(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    doc: &mut AutoCommit,
) -> Result<Landing, TransferError> {
    app_state::validate_document_namespace(package, workspace)?;
    let key = StorageKey::workspace_index(package, workspace);
    let _writer = writer(store, &key)?;
    if store
        .read(&key)?
        .is_some_and(|stored| !stored.bytes.is_empty())
    {
        return Err(RecordOpenError::AlreadyExists.into());
    }
    persist(store, &key, doc)
}

/// Merge every change `source` holds that `target` in `workspace` lacks — the
/// edits its old writer made after the transfer read it. Deterministic:
/// it is a CRDT merge, so the order it runs in cannot change the result.
/// Returns how many changes arrived, and the write bringing them when any
/// did.
///
/// # Errors
/// [`TransferError::Busy`] while the destination record is open in an app,
/// a missing or foreign record, or storage failure.
pub fn catch_up(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    target: &RecordCheckout,
    source: &AutoCommit,
) -> Result<(usize, Option<Landing>), TransferError> {
    let key = key(package, workspace, &target.id)?;
    let _writer = writer(store, &key)?;
    let doc = decode(store, &key)?;
    merge_into(store, &key, doc, target, workspace, source)
}

/// Merge into `doc`, held under `key`'s writer, what `source` adds, and
/// start that committing.
fn merge_into(
    store: &Rc<dyn DurableStore>,
    key: &StorageKey,
    mut doc: AutoCommit,
    target: &RecordCheckout,
    workspace: &str,
    source: &AutoCommit,
) -> Result<(usize, Option<Landing>), TransferError> {
    validate_identity(&doc, target, workspace)?;
    let arriving = doc.clone().get_changes_added(&mut source.clone()).len();
    if arriving == 0 {
        return Ok((0, None));
    }
    doc.merge_incoming(&mut source.clone())
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    validate_identity(&doc, target, workspace)?;
    let landing = persist(store, key, &mut doc)?;
    Ok((arriving, Some(landing)))
}

/// Stamp `origin` in `workspace` as retired: one last change in its own history,
/// after which it is never opened as a live document again. No write, and
/// no [`Landing`], for one already retired.
///
/// # Errors
/// [`TransferError::Busy`] while an app still holds the document, a missing
/// or foreign document, or storage failure.
pub fn retire(
    store: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
    origin: &Origin,
    why: &Retirement,
) -> Result<Option<Landing>, TransferError> {
    let key = origin.key(package, workspace)?;
    let _writer = writer(store, &key)?;
    let mut doc = decode(store, &key)?;
    if is_retired(&doc) {
        return Ok(None);
    }
    origin.validate(&doc, workspace)?;
    stamp_retired(&mut doc, why)?;
    persist(store, &key, &mut doc).map(Some)
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
    use crate::record::{RecordOwners, RecordStore, str_at, workspace_scope};
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

    /// The successor goes into the stamp under the key a record written
    /// before the rename carries it, and a stamp naming it for the workspace
    /// reads as the same successor.
    #[test]
    fn a_retirement_names_its_successor_under_the_stored_key() {
        let mut doc = AutoCommit::new();
        stamp_retired(&mut doc, &moved()).unwrap();
        let meta = meta_ro(&doc).expect("a stamped record has meta");
        let Some((Value::Object(ObjType::Map), retired)) = doc.get(&meta, RETIRED).unwrap() else {
            panic!("the retirement is a map");
        };
        assert!(doc.get(&retired, "room").unwrap().is_some(), "stored key");
        assert!(
            doc.get(&retired, "workspace").unwrap().is_none(),
            "one key, one spelling"
        );
        let mut renamed = doc.fork();
        renamed.put(&retired, "workspace", TO.to_owned()).unwrap();
        renamed.delete(&retired, "room").unwrap();
        renamed.commit();
        assert_eq!(retirement(&renamed), Some(moved()));
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
    fn a_moved_record_keeps_its_history_and_later_edits_and_leaves_its_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, old_writer) = seeded(temp.path());
        let origin = Origin::Record(checkout.clone());

        // The old app still holds the record: reading it needs no writer.
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let known = snapshot.clone().get_heads();
        let target = RecordId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let (installed, _) = install(&store, PACKAGE, TO, &mut arrived).unwrap();
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
        assert_eq!(
            catch_up(&store, PACKAGE, TO, &installed, &last).unwrap().0,
            1
        );
        assert_eq!(
            catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(),
            (0, None),
            "nothing more to write"
        );
        let _ = retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();

        let destination = read(&store, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("first draft, last words")
        );
        assert_eq!(workspace_scope(&destination).as_deref(), Some(TO));
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(RecordOpenError::Retired))
        ));
        let source_workspace = RecordOwners::new(temp.path().to_path_buf())
            .open(PACKAGE, FROM)
            .unwrap();
        assert!(matches!(
            source_workspace.try_open_record(&checkout),
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
    fn a_document_joins_another_workspace_only_as_exactly_the_minted_record_it_claims() {
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
            "scoped to the workspace it joins"
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
            "only a freshly minted id joins a workspace"
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

    /// A record's workspace is imported whether the document names it under
    /// the key every record is written with or the one a build named it by:
    /// it keeps the workspace it names, and the key it names it under. A key
    /// the import does not know is refused.
    #[test]
    fn a_document_imports_with_its_workspace_under_either_key() {
        let kind = RecordKind::new("transfer.test");
        for key in [crate::record::WORKSPACE, crate::record::WORKSPACE_ALIAS] {
            let target = RecordId::minted();
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "body", "a draft").unwrap();
            assert!(crate::record::init_scoped(
                &mut doc, &target, &kind, FROM, None
            ));
            let meta = meta_obj(&mut doc).unwrap();
            doc.put(&meta, key, FROM).unwrap();
            doc.commit();
            let bytes = doc.save();
            let (mut kept, identity) = validate_import(&bytes, FROM, &target, bytes.len()).unwrap();
            assert_eq!(identity.id, target, "{key}");
            assert_eq!(workspace_scope(&kept).as_deref(), Some(FROM), "{key}");
            let kept_meta = meta_obj(&mut kept).unwrap();
            assert_eq!(
                str_at(&kept, &kept_meta, key).as_deref(),
                Some(FROM),
                "the document keeps the key it named its workspace under"
            );
            kept.put(&kept_meta, "alias", "uuid:other").unwrap();
            let aliased = kept.save();
            assert!(
                validate_import(&aliased, FROM, &target, aliased.len()).is_err(),
                "a meta key it does not know is refused"
            );
        }
    }

    /// Carried into another workspace, a record names its new workspace under
    /// the one key every reader knows: the key a build named it by would name
    /// a second one.
    #[test]
    fn a_moved_record_names_its_new_workspace_under_the_stored_key() {
        let kind = RecordKind::new("transfer.test");
        let source = RecordCheckout::new(RecordId::minted(), kind.clone());
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", "a draft").unwrap();
        assert!(crate::record::init_scoped(
            &mut doc,
            &source.id,
            &kind,
            FROM,
            Some(FROM)
        ));
        let meta = meta_obj(&mut doc).unwrap();
        doc.delete(&meta, crate::record::WORKSPACE).unwrap();
        doc.put(&meta, crate::record::WORKSPACE_ALIAS, FROM)
            .unwrap();
        doc.commit();
        assert_eq!(workspace_scope(&doc).as_deref(), Some(FROM));

        let target = RecordId::minted();
        let moved = rehome(&doc, &Origin::Record(source), FROM, TO, &target).unwrap();
        assert_eq!(workspace_scope(&moved).as_deref(), Some(TO));
        let moved_meta = meta_obj(&mut moved.clone()).unwrap();
        assert_eq!(
            str_at(&moved, &moved_meta, crate::record::WORKSPACE),
            Some(TO.to_owned()),
            "the workspace it moved to, under the key every reader knows"
        );
        assert_eq!(
            str_at(&moved, &moved_meta, crate::record::WORKSPACE_ALIAS),
            None,
            "the key it was written under does not stay behind naming the workspace it left"
        );
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
            "a record of one workspace is never kept as another workspace's"
        );
        assert!(adopt_fetched(&here, PACKAGE, FROM, &checkout.id, b"not a document").is_err());
        let (kept, _) = adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes).unwrap();
        assert_eq!(kept, checkout);
        assert_eq!(
            adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes)
                .unwrap()
                .0,
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
            "a record that left its workspace is never brought back to life"
        );
    }

    /// On the frame loop a transfer neither reads the disk nor waits for a
    /// commit: it is told to come back while the store's own thread opens
    /// the document, and then only starts its write. Until the write lands
    /// the document stays the store's, so no other process takes it or
    /// reads it half-written, and the landing says when the next one may.
    #[test]
    fn a_transfer_on_the_frame_loop_starts_its_write_and_reports_when_it_lands() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, writer) = seeded(temp.path());
        drop(writer);
        let origin = Origin::Record(checkout.clone());
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &RecordId::minted()).unwrap();
        let (installed, landing) = {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            tilo_test_env::wait_for("the store's thread to open the record", || {
                match install(&store, PACKAGE, TO, &mut arrived) {
                    Ok(installed) => Some(installed),
                    Err(error) if error.is_opening() => None,
                    Err(error) => panic!("installing on the frame loop: {error}"),
                }
            })
        };
        let key = key(PACKAGE, TO, &installed.id).unwrap();
        let other_process = app_state::FsStore::new(temp.path());
        if other_process.lease(&key).is_ok() {
            assert_eq!(
                landing.durability(store.as_ref()),
                Durability::Durable,
                "the document is free only once the write is on the disk"
            );
        }
        store.flush(Some(&key)).unwrap();
        assert_eq!(landing.durability(store.as_ref()), Durability::Durable);
        assert_eq!(
            Landing::all(store.as_ref(), std::slice::from_ref(&landing)),
            Durability::Durable
        );
        let other_process: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(temp.path()));
        let there = read(&other_process, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(body(&there).as_deref(), Some("first draft"));
    }

    #[test]
    fn a_transfer_never_overwrites_or_crosses_into_its_own_workspace() {
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
            "a record is only read from the workspace it is scoped to"
        );
        let target = RecordId::minted();
        let mut first = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let _ = install(&store, PACKAGE, TO, &mut first).unwrap();
        let mut again = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        assert!(matches!(
            install(&store, PACKAGE, TO, &mut again),
            Err(TransferError::Open(RecordOpenError::AlreadyExists))
        ));
        let foreign = Origin::Record(RecordCheckout::new(target, checkout.kind.clone()));
        assert!(
            read(&store, PACKAGE, FROM, &foreign).is_err(),
            "a destination id means nothing in the source workspace"
        );
    }

    #[test]
    fn an_app_document_arrives_as_a_record_and_its_workspace_keeps_a_retired_one() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut app =
            app_state::DocStore::shared(Rc::clone(&store), StorageKey::app(PACKAGE)).unwrap();
        let mut doc = app.load_or_new();
        doc.put(ROOT, "body", "kept in the app document").unwrap();
        app.save(&mut doc).unwrap();

        let origin = Origin::AppDocument(RecordKind::new("tilo.app-document"));
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let target = RecordId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let (installed, _) = install(&store, PACKAGE, TO, &mut arrived).unwrap();
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
        let _ = retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(RecordOpenError::Retired))
        ));
    }
    /// A branch's copy keeps the record's id, where it was minted and its whole history, names the
    /// branch as its workspace, and writes as a writer of its own: the source and the copy go on
    /// apart.
    #[test]
    fn a_branch_keeps_the_record_and_its_history_and_names_the_branch() {
        let id = RecordId::minted();
        let mut source = AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut source,
            &id,
            &RecordKind::new("transfer.test"),
            FROM,
            Some(FROM),
        ));
        source.put(ROOT, "body", "plan").unwrap();
        source.commit();
        let before = source.get_heads();
        let mut copy = branch(&source, TO).unwrap();
        assert_eq!(workspace_scope(&copy).as_deref(), Some(TO));
        assert_eq!(
            workspace_scope(&source).as_deref(),
            Some(FROM),
            "the source stays"
        );
        assert_eq!(identity(&copy).map(|checkout| checkout.id), Some(id));
        assert_eq!(crate::record::home(&copy).as_deref(), Some(FROM));
        assert_eq!(body(&copy).as_deref(), Some("plan"));
        assert!(
            copy.get_changes(&[]).len() > source.get_changes(&[]).len(),
            "the copy holds the source's history and the change that names the branch"
        );
        assert!(copy.get_change_by_hash(&before[0]).is_some());
        assert_ne!(copy.get_actor(), source.get_actor(), "a writer of its own");
        copy.put(ROOT, "body", "branch").unwrap();
        copy.commit();
        source.put(ROOT, "body", "source").unwrap();
        source.commit();
        assert_eq!(body(&copy).as_deref(), Some("branch"));
        assert_eq!(body(&source).as_deref(), Some("source"));
        // A document no workspace scopes is forked as it is.
        let mut plain = AutoCommit::new();
        plain.put(ROOT, "body", "loose").unwrap();
        plain.commit();
        let mut forked = branch(&plain, TO).unwrap();
        assert_eq!(forked.get_heads(), plain.get_heads());
        assert!(branch(&plain, "").is_err());
    }

    /// A record of the workspace's own that any app makes from its kind alone where it holds none
    /// (the workspace's emoji) is, in a branch, the very record an app of the branch makes when it
    /// first opens it there: the two meet as one document, scoped to the branch, holding what the
    /// source held. Before, the branch's copy kept the source's genesis, the app's was the
    /// branch's, and the two met as a record of no identity, its history blocked.
    #[test]
    fn a_workspace_record_branched_is_the_one_an_app_of_the_branch_makes() {
        use crate::record::workspace::WorkspaceRecords;
        const CHAT: &str = "so.tilo.chat";
        const EMOJI: &str = "tilo.workspace.emoji";
        let initialize = |doc: &mut AutoCommit| {
            doc.put_object(ROOT, "emoji", ObjType::Map)
                .map(drop)
                .map_err(|error| error.to_string())
        };
        let records = |workspace: &str| {
            WorkspaceRecords::in_workspace(
                RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new())),
                workspace.into(),
                crate::permissions::UserId("ada".into()),
            )
        };
        let emoji = |doc: &AutoCommit| {
            let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, "emoji") else {
                return None;
            };
            match doc.get(&map, "parrot") {
                Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            }
        };
        let source = records(FROM).open_record(CHAT, EMOJI, &initialize).unwrap();
        source
            .edit(true, |doc| {
                let Ok(Some((_, map))) = doc.get(ROOT, "emoji") else {
                    return Err("no emoji".to_owned());
                };
                doc.put(&map, "parrot", "parrot.png")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let held = source.read(AutoCommit::clone).unwrap();
        let want = identity(&held).unwrap();

        let copy = branch(&held, TO).unwrap();
        assert_eq!(workspace_scope(&copy).as_deref(), Some(TO));
        assert_eq!(
            crate::record::home(&copy).as_deref(),
            Some(TO),
            "minted again"
        );
        assert_eq!(emoji(&copy).as_deref(), Some("parrot.png"));
        let made = records(TO)
            .open_record(CHAT, EMOJI, &initialize)
            .unwrap()
            .read(AutoCommit::clone)
            .unwrap();
        for (mut one, mut other) in [(made.clone(), copy.clone()), (copy.clone(), made.clone())] {
            one.merge_incoming(&mut other).unwrap();
            assert!(
                validate_identity(&one, &want, TO).is_ok(),
                "one record of the branch: {:?} in {:?}",
                identity(&one),
                workspace_scope(&one)
            );
            assert_eq!(emoji(&one).as_deref(), Some("parrot.png"));
        }
        assert_eq!(
            emoji(&held).as_deref(),
            Some("parrot.png"),
            "the source keeps its own"
        );
        assert_eq!(workspace_scope(&held).as_deref(), Some(FROM));
    }
}
