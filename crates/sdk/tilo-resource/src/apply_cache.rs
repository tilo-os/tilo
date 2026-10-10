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

use automerge::transaction::Transactable as _;
use automerge::{ActorId, ChangeHash, ROOT, ReadDoc};

use crate::delta::Delta;
use crate::model::{Doc, Fields};

/// Above this many changes since the kept version, or operations in them, a
/// whole read costs about what indexing them would.
const MAX_CHANGES: usize = 256;
const MAX_OPS: u64 = 50_000;

thread_local! {
    static KEPT: RefCell<HashMap<TypeId, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

struct Kept<R> {
    /// The record as read at `base`.
    record: R,
    /// The version `record` was read at, sorted.
    base: Vec<ChangeHash>,
    /// The change the action that kept `record` wrote on top of `base`: its
    /// actor and how many operations it holds. Not yet in `record`.
    wrote: Option<(ActorId, u64)>,
}

/// A record brought up to the document's current version, and the changes
/// since it was last written that its own actions did not make.
pub(crate) struct Warm<R> {
    pub(crate) record: R,
    pub(crate) dirty: Delta,
}

/// The record kept for `R`, brought up to the document as it stands, or
/// `None` when it must be read whole. Takes it: an action that fails keeps
/// nothing.
pub(crate) fn take<R: Fields + 'static>(doc: &Doc) -> Option<Warm<R>> {
    let kept = KEPT.with(|kept| kept.borrow_mut().remove(&TypeId::of::<R>()))?;
    let Kept {
        mut record,
        base,
        wrote,
    } = *kept.downcast::<Kept<R>>().ok()?;
    if doc.pending_ops() > 0 {
        return None;
    }
    let committed = doc.committed_document();
    let mut heads = doc.committed_heads();
    // An isolated document reads an older version than it holds.
    if heads != committed.get_heads() {
        return None;
    }
    heads.sort();
    if heads == base && wrote.is_none() {
        return Some(Warm {
            record,
            dirty: Delta::nothing(),
        });
    }
    if base
        .iter()
        .any(|hash| committed.get_change_meta_by_hash(hash).is_none())
    {
        return None;
    }
    let metas = committed.get_changes_meta(&base);
    let ops = metas.iter().try_fold(0_u64, |total, meta| {
        total.checked_add(meta.max_op.saturating_add(1).saturating_sub(meta.start_op))
    })?;
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
                deps.sort();
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
    record.refresh_fields(doc, &ROOT, &delta).ok()?;
    let dirty = match changes.get(..foreign) {
        Some([]) | None => Delta::nothing(),
        Some(foreign) => Delta::from_changes(doc, foreign, &|_| None),
    };
    Some(Warm { record, dirty })
}

/// Keep `record`, read at the version `doc`'s open transaction started from,
/// for the next action of its type. The transaction holds what the action
/// just wrote: every slot it wrote is as `store` writes it.
pub(crate) fn keep<R: 'static>(record: R, doc: &Doc) {
    if doc.committed_heads() != doc.committed_document().get_heads() {
        return;
    }
    let mut base = doc.base_heads();
    base.sort();
    let pending = doc.pending_ops();
    let wrote = if pending == 0 {
        None
    } else {
        let Ok(count) = u64::try_from(pending) else {
            return;
        };
        Some((doc.get_actor().clone(), count))
    };
    let kept: Box<dyn Any> = Box::new(Kept {
        record,
        base,
        wrote,
    });
    KEPT.with(|cell| {
        cell.borrow_mut().insert(TypeId::of::<R>(), kept);
    });
}

/// Drop every record kept on this thread, so the next action of each type
/// reads its record whole. For tests comparing the two paths.
#[doc(hidden)]
pub fn forget_all() {
    KEPT.with(|kept| kept.borrow_mut().clear());
}
