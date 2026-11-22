//! The record a `#[resource]` action started from, kept so the next action
//! of the same type starts from it too instead of reading the whole record
//! from the document again.
//!
//! An action on a record of a thousand items that changes one field would
//! otherwise read every item and write every field back, on the origin and
//! on every replica replaying it. Kept here is the record as read at the
//! version the last action started from, with how that action's own change
//! is recognised. The next action takes it, refreshes it with only the
//! changes since ([`Fields::refresh_fields`]), and writes by comparing with
//! it ([`Fields::write_changed_fields`]).
//!
//! The write must be the one a whole read and write would make, operation
//! for operation, because replicas replay the action and must reproduce its
//! change byte for byte, whether or not they have a record kept. A whole
//! write leaves every slot as `store` writes it, and so does a compared
//! write; what someone else wrote since (another replica, a schema older or
//! newer than this one) need not be, so their changes are handed to the
//! write as `dirty` and every slot they touched is written as `store`
//! writes it. Anything this module cannot vouch for, it does not keep: an
//! action that starts inside an open transaction, a version it cannot find
//! its own change on top of, more changes since than are worth indexing.
//!
//! Inside a transaction still open (a host applying several actions before it commits), the
//! record an action wrote is kept as it wrote it: every slot it holds is as `store` writes it,
//! so the next action of its type in the same transaction starts from it with nothing to
//! refresh, as long as nothing else wrote into the transaction in between. Once that
//! transaction commits as one change of what was written into it (a group's members, written
//! once), what was kept is the document as the change left it: the next action reads only what
//! others wrote beside it, not the group's own change back.
//!
//! A write also keeps the items it stored whole into new entries, as each
//! reads back, for the view that follows its change and for the next action
//! of its type: when that change is all that happened since, they take them
//! rather than reading each back ([`taking_stored`]), and the size of the
//! change is no reason to read everything again.
//!
//! An app's model module keeps nothing: it applies one action per call, to a
//! document the device holds (`--cfg tilo_model_guest`), so there is no next
//! action to keep anything for, and its writes are the whole ones.
//!
//! Keyed by the record's type and by content-addressed change hashes, so a
//! record kept for one document is never mistaken for another's: a document
//! without the kept version reads the record whole.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable as _;
use automerge::{ActorId, ChangeHash, ObjId, ReadDoc};

use crate::delta::Delta;
use crate::model::Doc;

/// Above this many changes since the kept version, or operations in them, a
/// whole read costs about what indexing them would.
const MAX_CHANGES: usize = 256;
const MAX_OPS: u64 = 50_000;

thread_local! {
    static KEPT: RefCell<HashMap<TypeId, Kept>> = RefCell::new(HashMap::new());
}

struct Kept {
    /// The record as read at `base`.
    record: Box<dyn Any>,
    /// The version `record` was read at, sorted.
    base: Vec<ChangeHash>,
    /// The change the action that kept `record` wrote on top of `base`. Not
    /// yet in `record`.
    wrote: Wrote,
    /// Kept inside a transaction still open: its actor and how many operations it held once
    /// `record` was written into it. `record` is then the record as written, not as read.
    open: Option<(ActorId, usize)>,
}

/// The record kept for the record type `kind`, brought up to the document
/// as it stands by `refresh`, and the changes since it was last written that
/// its own actions did not make; `None` when it must be read whole. Takes
/// it: an action that fails keeps nothing.
pub(crate) fn take_any(
    kind: TypeId,
    doc: &Doc,
    refresh: &dyn Fn(&mut dyn Any, &Doc, &Delta) -> bool,
) -> Option<(Box<dyn Any>, Delta)> {
    if cfg!(tilo_model_guest) {
        return None;
    }
    let Kept {
        record: mut state,
        base,
        mut wrote,
        open,
    } = KEPT.with(|kept| kept.borrow_mut().remove(&kind))?;
    // What was kept as written into a transaction holds its own change.
    let holds_person_workspace = open.is_some();
    if let Some((actor, pending)) = open {
        let mut started = doc.base_heads();
        started.compact_sort();
        // The same transaction, with nothing written into it since.
        if doc.pending_ops() > 0 {
            return (started == base && doc.get_actor() == &actor && doc.pending_ops() == pending)
                .then(|| (state, Delta::nothing()));
        }
        // The transaction committed since, as one change of exactly what was
        // written into it when it was kept: it is the document as that change
        // left it, and only what others wrote beside it is read.
        wrote = (pending > 0)
            .then(|| u64::try_from(pending).ok().map(|count| (actor, count)))
            .flatten();
    }
    if doc.pending_ops() > 0 {
        return None;
    }
    let committed = doc.committed_document();
    let mut heads = doc.committed_heads();
    // An isolated document reads an older version than it holds.
    if heads != committed.get_heads() {
        return None;
    }
    heads.compact_sort();
    if heads == base && wrote.is_none() {
        return Some((state, Delta::nothing()));
    }
    if base
        .iter()
        .any(|hash| committed.get_change_meta_by_hash(hash).is_none())
    {
        return None;
    }
    let metas = committed.get_changes_meta(&base);
    if metas.len() > MAX_CHANGES {
        return None;
    }
    let mut ops = 0_u64;
    let mut own = None;
    let mut own_meta = None;
    let mut changes = Vec::with_capacity(metas.len());
    for meta in &metas {
        let count = meta.max_op.saturating_add(1).saturating_sub(meta.start_op);
        let ours = own_meta.is_none()
            && wrote.as_ref().is_some_and(|(actor, wrote)| {
                let mut deps = meta.deps.clone();
                deps.compact_sort();
                meta.actor.as_ref() == actor && deps == base && count == *wrote
            });
        if ours {
            own_meta = Some((meta, count));
            if !holds_person_workspace {
                own = Some(committed.get_change_by_hash(&meta.hash)?);
            }
        } else {
            ops = ops.checked_add(count)?;
            changes.push(committed.get_change_by_hash(&meta.hash)?);
        }
    }
    // The change the last action wrote is not here: it was rolled back, or
    // this is another document.
    if wrote.is_some() && own_meta.is_none() {
        return None;
    }
    let foreign = changes.len();
    // Only what is read is counted: a copy holding its own change reads none
    // of it, and its own change alone is taken as it was written, every item
    // it stored as it reads back, rather than read.
    let alone = foreign == 0 && own_meta.is_some_and(|(meta, _)| stored_by(meta).is_some());
    if let Some((_, count)) = own_meta.filter(|_| !holds_person_workspace && !alone) {
        ops = ops.checked_add(count)?;
    }
    if ops > MAX_OPS {
        return None;
    }
    changes.extend(own);
    if changes.is_empty() {
        return Some((state, Delta::nothing()));
    }
    let delta = Delta::from_changes(doc, &changes, &|_| None);
    if delta.is_everything() {
        return None;
    }
    let refreshed = if alone {
        taking_stored(doc, || refresh(state.as_mut(), doc, &delta))
    } else {
        refresh(state.as_mut(), doc, &delta)
    };
    if !refreshed {
        return None;
    }
    let dirty = match changes.get(..foreign) {
        Some([]) | None => Delta::nothing(),
        Some(foreign) => Delta::from_changes(doc, foreign, &|_| None),
    };
    Some((state, dirty))
}

/// Keep `record`, of the record type `kind`, read at `started`, the version
/// the action began from, for the next action of its type. The transaction holds what the action just
/// wrote: every slot it wrote is as `store` writes it. An action that merged
/// a change of its own on the way (a deterministic slot made beside it)
/// moved its base past what `record` holds, so nothing is kept.
pub(crate) fn keep_any(kind: TypeId, record: Box<dyn Any>, doc: &Doc, started: &[ChangeHash]) {
    if cfg!(tilo_model_guest) {
        return;
    }
    if let Some((base, wrote)) = kept_base(doc, started) {
        let kept = Kept {
            record,
            base,
            wrote,
            open: None,
        };
        KEPT.with(|cell| {
            cell.borrow_mut().insert(kind, kept);
        });
    }
}

/// Keep `record`, of the record type `kind`, exactly as an action inside a transaction still
/// open just wrote it into `doc`, for the next action of its type in the same transaction.
pub(crate) fn keep_open(kind: TypeId, record: Box<dyn Any>, doc: &Doc) {
    if cfg!(tilo_model_guest) {
        return;
    }
    let mut base = doc.base_heads();
    base.compact_sort();
    let kept = Kept {
        record,
        base,
        wrote: None,
        open: Some((doc.get_actor().clone(), doc.pending_ops())),
    };
    KEPT.with(|cell| {
        cell.borrow_mut().insert(kind, kept);
    });
}

/// The change an action wrote on top of the version it started from: its
/// actor and how many operations it holds.
type Wrote = Option<(ActorId, u64)>;

/// The version a record read at `started` is kept at and the change on top
/// of it the action wrote, or `None` when it cannot be kept.
fn kept_base(doc: &Doc, started: &[ChangeHash]) -> Option<(Vec<ChangeHash>, Wrote)> {
    if doc.committed_heads() != doc.committed_document().get_heads() {
        return None;
    }
    if doc.base_heads() != started {
        return None;
    }
    let mut base = doc.base_heads();
    base.compact_sort();
    let pending = doc.pending_ops();
    let wrote = if pending == 0 {
        None
    } else {
        Some((doc.get_actor().clone(), u64::try_from(pending).ok()?))
    };
    Some((base, wrote))
}

/// Drop every record kept on this thread, so the next action of each type
/// reads its record whole. For tests comparing the two paths.
#[doc(hidden)]
pub fn forget_all() {
    KEPT.with(|kept| kept.borrow_mut().clear());
    STORED.with(|stored| stored.borrow_mut().clear());
}

/// The items an action's write stored whole into new entries of a
/// collection, each by the entry object it was stored under (an `Arc` of the
/// item, as the collection holds it), with the change that holds exactly what
/// was written: its actor, the version it was made on, sorted, and how many
/// operations it holds. A reader that follows exactly that change takes these
/// items as they are rather than reading each back from the document
/// ([`taking_stored`]). Each is noted as it reads back: one holding rich
/// text, which reads back as the document's own structure, with that read
/// again from where it was written ([`crate::table::settle`]).
struct Stored {
    actor: ActorId,
    base: Vec<ChangeHash>,
    ops: u64,
    items: StoredItems,
}

/// The items one write stored, by the entry object each was stored under.
type StoredItems = std::rc::Rc<HashMap<ObjId, Box<dyn Any>>>;

/// How many writes' items are kept for their readers.
const STORED_KEPT: usize = 4;

thread_local! {
    /// The items noted by the write under way on this thread, if there is one.
    static STORING: RefCell<Option<HashMap<ObjId, Box<dyn Any>>>> = const { RefCell::new(None) };
    /// The items of the latest writes, newest last.
    static STORED: RefCell<Vec<Stored>> = const { RefCell::new(Vec::new()) };
    /// The items of the change the reader on this thread is following now.
    static TAKING: RefCell<Option<StoredItems>> = const { RefCell::new(None) };
}

/// Note the items the write about to start stores ([`note_stored`]), until
/// [`seal_stored`]: from nothing, as what a write that failed noted names
/// objects its rolled back transaction never made.
pub(crate) fn begin_storing() {
    STORING.with(|storing| *storing.borrow_mut() = Some(HashMap::new()));
}

/// The item `shared` (an `Arc` of it) was stored whole under the new entry
/// object `entry`, if a write notes what it stores.
pub(crate) fn note_stored(entry: &ObjId, shared: &dyn Fn() -> Option<Box<dyn Any>>) {
    STORING.with(|storing| {
        if let Some(storing) = storing.borrow_mut().as_mut()
            && let Some(shared) = shared()
        {
            storing.insert(entry.clone(), shared);
        }
    });
}

/// The write is done: what it noted is kept for the change `doc`'s open
/// transaction holds now, if that transaction commits as it stands.
pub(crate) fn seal_stored(doc: &Doc) {
    let Some(items) = STORING.with(|storing| storing.borrow_mut().take()) else {
        return;
    };
    let ops = u64::try_from(doc.pending_ops()).unwrap_or(0);
    if items.is_empty() || ops == 0 {
        return;
    }
    let mut base = doc.base_heads();
    base.compact_sort();
    let stored = Stored {
        actor: doc.get_actor().clone(),
        base,
        ops,
        items: std::rc::Rc::new(items),
    };
    STORED.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() >= STORED_KEPT {
            kept.remove(0);
        }
        kept.push(stored);
    });
}

/// Call `follow`, a reader bringing its state up to date with `doc`, taking
/// the items a write on this thread stored when that write's change is the
/// only head `doc` has: every other change is one it was made on, which
/// cannot touch the entries it made, so every item it stored is still as it
/// was written. However the reader reads: the changes since its last sync,
/// or the whole document (a part read for the first time, a change too large
/// to apply as a delta).
pub(crate) fn taking_stored<T>(doc: &Doc, follow: impl FnOnce() -> T) -> T {
    let Some(items) = stored_at_head(doc) else {
        return follow();
    };
    let before = TAKING.with(|taking| taking.borrow_mut().replace(items));
    let followed = follow();
    TAKING.with(|taking| *taking.borrow_mut() = before);
    followed
}

/// The items stored by the write on this thread whose change is `doc`'s only
/// head, if one noted any.
fn stored_at_head(doc: &Doc) -> Option<StoredItems> {
    if doc.pending_ops() > 0 || STORED.with(|stored| stored.borrow().is_empty()) {
        return None;
    }
    let heads = doc.committed_heads();
    let [head] = heads.as_slice() else {
        return None;
    };
    stored_by(&doc.committed_document().get_change_meta_by_hash(head)?)
}

/// The items the write on this thread whose change is `meta` stored, if one
/// noted any.
pub(crate) fn stored_by(meta: &automerge::ChangeMetadata<'_>) -> Option<StoredItems> {
    let ops = meta.max_op.saturating_add(1).saturating_sub(meta.start_op);
    let mut deps = meta.deps.clone();
    deps.compact_sort();
    STORED.with(|stored| {
        stored
            .borrow()
            .iter()
            .rev()
            .find(|stored| stored.ops == ops && *meta.actor == stored.actor && stored.base == deps)
            .map(|stored| std::rc::Rc::clone(&stored.items))
    })
}

/// Hand `take` the item stored under the new entry object `entry` by the
/// change the reader follows, if it stored one there. Whether `take` took it.
pub(crate) fn take_stored(entry: &ObjId, take: &mut dyn FnMut(&dyn Any) -> bool) -> bool {
    TAKING.with(|taking| {
        taking
            .borrow()
            .as_ref()
            .and_then(|items| items.get(entry))
            .is_some_and(|stored| take(stored.as_ref()))
    })
}
