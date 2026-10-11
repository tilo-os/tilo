//! A list someone arranges, of items that carry their own ids: [`Ordered`].
//!
//! [`Collection`](crate::Collection) keeps each item's id beside it, which
//! is right when the id is only the collection's business. Items that carry
//! their id themselves ([`Identified`]) — an effect in a clip's stack, a step
//! in a recipe — read best as a plain vector, so `Ordered<T>` holds them as a
//! `Vec<T>` in memory and stores them exactly as a `Collection` is stored:
//! a map from id to `{ k: order key, v: value }`. Editing an item writes only
//! that item, a move rewrites one order key, concurrent inserts interleave
//! deterministically and a delete never disturbs another item's identity —
//! none of which a list stored by position can promise.

use automerge::ObjId;

use crate::collection::{Id, VALUE, store_in_order, stored_entries};
use crate::keyed::{Identified, ItemStorage, Items, removed_and_changed};
use crate::model::LocatedAt as _;
use crate::model::{Doc, Model, ModelError, Slot};
use crate::path::{Edit, EditOp, Path, Segment};

/// Items in the order they were arranged, stored under the ids they carry.
/// See the module docs.
pub type Ordered<T> = Items<T, Arranged>;

/// [`Ordered`] storage: a map from id to `{ k: order key, v: value }`, as a
/// [`Collection`](crate::Collection) is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arranged;

impl<R, T: Model + Identified> Path<R, Ordered<T>> {
    /// Add `item` under its own id at position `at`.
    #[must_use]
    pub fn insert(&self, at: usize, item: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: Some(item.id()),
            at,
            value: item.to_json(),
        })
    }

    /// Move the item `id` to position `at`.
    #[must_use]
    pub fn move_to(&self, id: &Id, at: usize) -> Edit<R> {
        self.edit(EditOp::MoveTo { id: id.clone(), at })
    }
}

impl ItemStorage for Arranged {
    const ARRANGED: bool = true;
    const NAME: &'static str = "arranged items";

    #[inline(never)]
    fn load<T: Model + Identified>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError> {
        stored_entries(doc, map)?
            .into_iter()
            .map(|stored| {
                crate::model::load_slot::<T>(doc, stored.value, VALUE).located(stored.id.as_str())
            })
            .collect()
    }

    #[inline(never)]
    fn store<T: Model + Identified>(
        items: &[T],
        doc: &mut Doc,
        slot: Slot<'_>,
    ) -> Result<(), ModelError> {
        let ids: Vec<Id> = items.iter().map(Identified::id).collect();
        let items: Vec<(&Id, &dyn crate::model::Stores)> = ids
            .iter()
            .zip(items)
            .map(|(id, item)| (id, item as &dyn crate::model::Stores))
            .collect();
        store_in_order(doc, slot, &items)
    }

    #[inline(never)]
    fn diff<T: Model + Identified, R>(
        before: &[T],
        after: &[T],
        path: &[Segment],
        out: &mut Vec<Edit<R>>,
    ) {
        removed_and_changed(before, after, path, out);
        let present = |id: &Id| after.iter().any(|item| &item.id() == id);
        // Walk `after` against the order the edits so far leave: what
        // survived of `before`, then each item put where `after` has it.
        let mut order: Vec<Id> = before
            .iter()
            .map(Identified::id)
            .filter(|id| present(id))
            .collect();
        for (index, item) in after.iter().enumerate() {
            let id = item.id();
            match order.iter().position(|candidate| candidate == &id) {
                Some(now) if now == index => {}
                Some(now) => {
                    order.remove(now);
                    order.insert(index.min(order.len()), id.clone());
                    out.push(Edit::new(path.to_vec(), EditOp::MoveTo { id, at: index }));
                }
                None => {
                    order.insert(index.min(order.len()), id.clone());
                    out.push(Edit::new(
                        path.to_vec(),
                        EditOp::InsertItem {
                            id: Some(id),
                            at: index,
                            value: item.to_json(),
                        },
                    ));
                }
            }
        }
    }
}
