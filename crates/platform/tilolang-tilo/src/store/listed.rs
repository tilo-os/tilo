//! What a listing read, kept until something changes: the listing a view's rows read again
//! rather than the index, and what a change keeps of the checks it made
//! ([`Filtered::spliced`]).

use std::collections::BTreeSet;
use std::rc::Rc;

use tilolang_runtime::store::{ANY_FIELD, FieldKey, Listing, Pred, doc_of};

use super::ItemTouches;
use crate::index::{self as idx, merged::Filtered};

/// A listing read once in this state of the store: until anything changes, a view's rows read
/// it again rather than the index.
pub(super) struct ListedOnce {
    pub(super) resource: String,
    pub(super) field: String,
    pub(super) pred: Option<Pred>,
    pub(super) order: Option<tilolang_runtime::query::Order>,
    pub(super) listing: Rc<Listing>,
    /// The run it checked, when the read can be made again the same way: what a later change
    /// keeps of its checks ([`Filtered::spliced`]).
    pub(super) filtered: Option<Rc<Filtered>>,
    /// The items the changes since it was read wrote, when that is all they wrote of it.
    pub(super) carry: Option<BTreeSet<String>>,
}

impl ListedOnce {
    /// Whether a change of `key` may change this listing: its collection, the resource holding
    /// it as a whole, or, for a filtered or sorted listing, the field of an item of its record
    /// that the filter or order reads (any item: one in another collection may move into this
    /// one). An item's other fields leave which items it lists, and their order, as they were.
    pub(super) fn reached_by(&self, key: &FieldKey) -> bool {
        (key.resource == self.resource && (key.field == self.field || key.field == ANY_FIELD))
            || (self.reads_item(key) && (self.pred.is_some() || self.order.is_some()))
    }

    /// Whether `key` is a field of an item of the record this listing lists that its filter or
    /// its order reads.
    fn reads_item(&self, key: &FieldKey) -> bool {
        let item = key.resource.contains('/') && key.doc() == doc_of(&self.resource);
        let reads = |id: &str| id == key.field;
        item && (key.field == ANY_FIELD
            || self
                .pred
                .as_ref()
                .is_some_and(|pred| pred.fields().iter().any(|read| reads(&read.spec.id)))
            || self.order.as_ref().is_some_and(|order| {
                // Any change of an item is its last version.
                reads(&order.field) || order.field == idx::reflect::UPDATED
            }))
    }

    /// The items a change of `key` wrote whose fields decide whether they are listed and where:
    /// what the listing checks again rather than all of it. `None` when it read no run it could
    /// check again, or the change does not name the items it wrote.
    pub(super) fn touched_by(
        &self,
        key: &FieldKey,
        items: &ItemTouches,
    ) -> Option<BTreeSet<String>> {
        self.filtered.as_ref()?;
        // An item put in or taken out of the collection it lists.
        if key.resource == self.resource && key.field == self.field {
            return items.get(key).cloned();
        }
        if !self.reads_item(key) {
            return None;
        }
        // The item whose field it is, by the id the listing holds it under.
        let id = idx::derive::id_of(&key.resource)?;
        Some(BTreeSet::from([id.to_owned()]))
    }
}
