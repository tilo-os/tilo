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
//! Keyed by the record's type and by content-addressed change hashes, so a
//! record kept for one document is never mistaken for another's: a document
//! without the kept version reads the record whole.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable as _;
use automerge::{ActorId, ChangeHash, ReadDoc};

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
    let Kept {
        mut record,
        base,
        wrote,
    } = KEPT.with(|kept| kept.borrow_mut().remove(&kind))?;
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
        return Some((record, Delta::nothing()));
    }
    if base
        .iter()
        .any(|hash| committed.get_change_meta_by_hash(hash).is_none())
    {
        return None;
    }
    let metas = committed.get_changes_meta(&base);
    let mut ops = 0_u64;
    for meta in &metas {
        ops = ops.checked_add(meta.max_op.saturating_add(1).saturating_sub(meta.start_op))?;
    }
    if metas.len() > MAX_CHANGES || ops > MAX_OPS {
        return None;
    }
    let mut own = None;
    let mut changes = Vec::with_capacity(metas.len());
    for meta in &metas {
        let change = committed.get_change_by_hash(&meta.hash)?;
        let ours = own.is_none()
            && wrote.as_ref().is_some_and(|(actor, count)| {
                let mut deps = meta.deps.clone();
                deps.compact_sort();
                meta.actor.as_ref() == actor
                    && deps == base
                    && meta.max_op.saturating_add(1).saturating_sub(meta.start_op) == *count
            });
        if ours {
            own = Some(change);
        } else {
            changes.push(change);
        }
    }
    // The change the last action wrote is not here: it was rolled back, or
    // this is another document.
    if wrote.is_some() && own.is_none() {
        return None;
    }
    let foreign = changes.len();
    changes.extend(own);
    let delta = Delta::from_changes(doc, &changes, &|_| None);
    if delta.is_everything() {
        return None;
    }
    if !refresh(record.as_mut(), doc, &delta) {
        return None;
    }
    let dirty = match changes.get(..foreign) {
        Some([]) | None => Delta::nothing(),
        Some(foreign) => Delta::from_changes(doc, foreign, &|_| None),
    };
    Some((record, dirty))
}

/// Keep `record`, of the record type `kind`, read at `started`, the version
/// the action began from, for the next action of its type. The transaction holds what the action just
/// wrote: every slot it wrote is as `store` writes it. An action that merged
/// a change of its own on the way (a deterministic slot made beside it)
/// moved its base past what `record` holds, so nothing is kept.
pub(crate) fn keep_any(kind: TypeId, record: Box<dyn Any>, doc: &Doc, started: &[ChangeHash]) {
    if let Some((base, wrote)) = kept_base(doc, started) {
        let kept = Kept {
            record,
            base,
            wrote,
        };
        KEPT.with(|cell| {
            cell.borrow_mut().insert(kind, kept);
        });
    }
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
}
