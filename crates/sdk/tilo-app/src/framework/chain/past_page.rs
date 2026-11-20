//! The items a chain's parts keep past the page their publication carries.
//!
//! A part publishes a page of each collection it keeps ([`ITEM_PAGE`]); what
//! it holds past that page is resolved one by one when addressed, and listed
//! here from the parts this device holds, so a listing of a chain reaches
//! every item however many one part holds, as a listing of an app's own
//! resource reaches the items past its page
//! ([`super::super::root::past_page`]).
//!
//! [`ITEM_PAGE`]: tilo_resource::items::ITEM_PAGE

use super::*;

/// One item a part keeps past the page it publishes: the part's state, the
/// collection it stands in and its id.
struct PastItem {
    state: Rc<dyn Any>,
    field: &'static str,
    id: tilo_resource::Id,
}

impl ChainState {
    /// Every item the held parts keep past the page each publishes, newest
    /// part first — the order [`Self::publish_shared`] carries the parts'
    /// pages in — and within a part the order its collections keep: what a
    /// page of a listing past those pages reads. An item the chain does not
    /// show ([`Chained::hidden`]) is left out.
    fn past_page_each(&self) -> Vec<PastItem> {
        let part = self.part();
        let Some(table) = part.table else {
            return Vec::new();
        };
        let parts = self.parts();
        let hidden = (part.hidden)(&*(part.typed)(&parts));
        let mut beyond = Vec::new();
        for (_, state) in parts.iter().rev() {
            let published = (part.children)(&**state);
            let every = tilo_resource::table::resource_items(table, &**state);
            for collection in published
                .items
                .iter()
                .filter(|collection| collection.within.is_empty())
            {
                let Some((_, items)) = every.iter().find(|(field, _)| *field == collection.field)
                else {
                    continue;
                };
                for (id, _) in items.iter().skip(collection.items.len()) {
                    let resource =
                        tilo_resource::items::item_resource_id_in(part.kind, collection.field, id);
                    if hidden.contains(&resource) {
                        continue;
                    }
                    beyond.push(PastItem {
                        state: Rc::clone(state),
                        field: collection.field,
                        id: id.clone(),
                    });
                }
            }
        }
        beyond
    }

    /// How many items the held parts keep past the page each publishes
    /// ([`Self::past_page_window`]): counted without building them.
    pub(crate) fn past_page_len(&self) -> usize {
        self.past_page_each().len()
    }

    /// The items the held parts keep past the page each publishes, from
    /// `offset` on, at most `take`, each built alone from the part keeping
    /// it and carried out where the chain is: a listing pays its window, not
    /// every item the chain holds.
    pub(crate) fn past_page_window(
        &self,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        let part = self.part();
        let Some(table) = part.table else {
            return Vec::new();
        };
        self.past_page_each()
            .into_iter()
            .skip(offset)
            .take(take)
            .filter_map(|item| {
                let _built = tilo_resource::profiling::Phase::start("framework.past_item");
                let resource =
                    tilo_resource::items::item_resource_id_in(part.kind, item.field, &item.id);
                let child = (part.child)(&*item.state, &resource)?;
                Some(
                    self.core
                        .carried_item(route_at(child, &self.core.id, true), Some(table)),
                )
            })
            .collect()
    }
}
