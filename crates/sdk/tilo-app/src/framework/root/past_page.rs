//! The items a record publishes past the page its tree carries.
//!
//! A record of thousands publishes a page of its items in its tree
//! (`ITEM_PAGE`); what it leaves out is resolved here, item by item, so a
//! listing reaches every one of them without the page carrying them: by
//! name, by window, and by count.
use super::ResourceRoot;
use super::read::offer_document_reads;
use super::{ResourceSpec, offer_item_open};
use ui_core::automerge::AutoCommit;

impl ResourceRoot {
    /// The items of each collection past the page the published tree
    /// carries, each resolved by id as [`Self::resource_for`] resolves one
    /// the tree leaves out: what a listing reads to reach every item.
    pub(super) fn past_page_all(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        let Some(table) = self.resource.record_table else {
            return Vec::new();
        };
        let current = if self.typed.follows(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let parts = self.typed.children_parts(&*state);
        let every = tilo_resource::table::record_items(table, &*state);
        let mut children = Vec::new();
        for collection in parts
            .items
            .iter()
            .filter(|collection| collection.within.is_empty())
        {
            let Some((_, items)) = every.iter().find(|(field, _)| *field == collection.field)
            else {
                continue;
            };
            for (id, _) in items.iter().skip(collection.items.len()) {
                let resource_id = (self.resource.item_id)(collection, id);
                let Some(mut child) = self.typed.child(&*state, doc, &resource_id) else {
                    continue;
                };
                if self.resource.readable {
                    offer_document_reads(&mut child, &|id| self.typed.document_child(&*state, id));
                }
                children.push(child);
            }
        }
        self.bind_row_declarations(&mut children);
        if self.stages.opens() {
            for child in &mut children {
                offer_item_open(self.resource.opens_as_item, child);
            }
        }
        children
    }

    /// [`Self::resources_past_page`] counted without building it: how many
    /// items the published pages leave out, across the record's collections.
    pub(super) fn past_page_count(&self, doc: &AutoCommit) -> usize {
        let Some(table) = self.resource.record_table else {
            return 0;
        };
        let current = if self.typed.follows(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let parts = self.typed.children_parts(&*state);
        let every = tilo_resource::table::record_items(table, &*state);
        parts
            .items
            .iter()
            .filter(|collection| collection.within.is_empty())
            .map(|collection| {
                every
                    .iter()
                    .find(|(field, _)| *field == collection.field)
                    .map_or(0, |(_, items)| {
                        items.len().saturating_sub(collection.items.len())
                    })
            })
            .sum()
    }

    /// [`Self::resources_past_page`] as id and label pairs, read from the
    /// collections' items without building a resource for each: what a name
    /// lookup scans.
    pub(super) fn past_page_names(&self, doc: &AutoCommit) -> Vec<(String, String)> {
        let Some(table) = self.resource.record_table else {
            return Vec::new();
        };
        let current = if self.typed.follows(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let parts = self.typed.children_parts(&*state);
        let every = tilo_resource::table::record_items(table, &*state);
        let mut labels = Vec::new();
        for collection in parts
            .items
            .iter()
            .filter(|collection| collection.within.is_empty())
        {
            let Some((_, items)) = every.iter().find(|(field, _)| *field == collection.field)
            else {
                continue;
            };
            for (id, label) in items.iter().skip(collection.items.len()) {
                labels.push(((self.resource.item_id)(collection, id), label.clone()));
            }
        }
        labels
    }

    /// The items [`Self::resources_past_page`] leaves out from `offset` on,
    /// at most `take`, built alone: a listing paging a record of thousands
    /// pays its window.
    pub(super) fn past_page_window(
        &self,
        doc: &AutoCommit,
        offset: usize,
        take: usize,
    ) -> Vec<ResourceSpec> {
        let Some(table) = self.resource.record_table else {
            return Vec::new();
        };
        let current = if self.typed.follows(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let parts = self.typed.children_parts(&*state);
        let every = tilo_resource::table::record_items(table, &*state);
        let mut children = Vec::new();
        let mut skipped = 0usize;
        for collection in parts
            .items
            .iter()
            .filter(|collection| collection.within.is_empty())
        {
            if children.len() >= take {
                break;
            }
            let Some((_, items)) = every.iter().find(|(field, _)| *field == collection.field)
            else {
                continue;
            };
            let past = items.len().saturating_sub(collection.items.len());
            if offset >= skipped.saturating_add(past) {
                skipped = skipped.saturating_add(past);
                continue;
            }
            let start = offset.saturating_sub(skipped);
            let room = take.saturating_sub(children.len());
            for (id, _) in items
                .iter()
                .skip(collection.items.len())
                .skip(start)
                .take(room)
            {
                let resource_id = (self.resource.item_id)(collection, id);
                let Some(mut child) = self.typed.child(&*state, doc, &resource_id) else {
                    continue;
                };
                if self.resource.readable {
                    offer_document_reads(&mut child, &|id| self.typed.document_child(&*state, id));
                }
                children.push(child);
            }
            skipped = skipped.saturating_add(past);
        }
        self.bind_row_declarations(&mut children);
        if self.stages.opens() {
            for child in &mut children {
                offer_item_open(self.resource.opens_as_item, child);
            }
        }
        children
    }
}
