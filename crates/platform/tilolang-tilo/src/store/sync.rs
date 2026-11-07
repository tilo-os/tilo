//! Bringing the store up to date with the workspace record: its new changes applied to the
//! mirror and derived into every index read from, and what they changed told to those who read
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_automerge::ApplyIncoming as _;
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange};

use super::{
    Doc, Ends, Inner, ItemTouches, TiloStore, changed_by, collections_by_doc, everything,
    mirror_of, name_version_changes, note_derived, note_versions, record_splices,
    write_as_a_known_actor,
};
use crate::chunk::{self, ChunkNo};
use crate::index::derive::Reached;
use crate::index::overlay::Overlay;
use crate::index::view::{self, View};
use crate::index::{self as idx};
use crate::reads::Changed;

impl TiloStore {
    /// Bring the store up to date with the workspace record: when the record's heads moved, the
    /// store's own saved writes are dropped from the mirror and the overlay (the record holds
    /// them now, or will) — the ones an origin derives stay, as the record never holds them —
    /// the record's new changes are applied to the mirror and derived into each index, and each
    /// subscriber is told which fields of which resources changed. A store without the record
    /// takes it, and catches each index up from its heads. Returns whether the record had
    /// moved.
    pub fn sync_from(&self, record: &Doc) -> bool {
        self.sync_document(chunk::FIRST, record)
    }

    /// [`Self::sync_from`] for the chunk `chunk` of the record ([`crate::chunk`]): the store
    /// holds its document beside the record's, reading and writing there the resources the
    /// record's manifest places in it. Returns whether that chunk had moved.
    pub fn sync_chunk(&self, chunk: ChunkNo, doc: &Doc) -> bool {
        self.sync_document(chunk, doc)
    }

    fn sync_document(&self, chunk: ChunkNo, doc: &Doc) -> bool {
        let mut inner = self.mutate();
        if inner.chunks.mirror(chunk).is_none() {
            inner.attach_document(chunk, doc);
            drop(inner);
            self.changing().notify_all();
            return true;
        }
        let heads = doc.committed_document().get_heads();
        if inner.chunks.seen(chunk) == heads.as_slice() {
            return false;
        }
        let before = inner.index.clone();
        let from = inner.chunks.heads();
        let old_overlay = std::mem::take(&mut inner.overlay);
        let old_snapshots: BTreeMap<AppId, Arc<idx::Snapshot>> = inner
            .apps
            .iter()
            .filter(|(_, held)| held.ready())
            .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
            .collect();
        // The store's own writes leave the mirror: the items they added, removed or moved are
        // moved again below, as the record has them. What an origin derives is never the
        // record's to have, so those writes stay, and are applied again on the record's new
        // copy below.
        let mut items = inner.drop_committed_local(chunk);
        inner.rollback(chunk);
        let (changed, ends, placed) = inner.apply_new_changes(chunk, doc, &before);
        inner.chunks.set_seen(chunk, heads);
        let Some(changed) = changed else {
            // The mirror held a change of its own, which only a copy of the record removes.
            inner.versions = None;
            inner.nested.clear();
            inner.stats.full_syncs = inner.stats.full_syncs.saturating_add(1);
            inner.chunks.insert(chunk, mirror_of(doc));
            if chunk == chunk::FIRST {
                inner.chunks.reread_placement();
            }
            inner.rescan();
            // What an origin derives is not the record's: it is written onto the fresh copy,
            // and derived again over the indexes the record's changes left, so reads keep the
            // items it holds where they are.
            inner.reapply_derived(chunk);
            let reached = everything(&before, &inner.index);
            inner.derive_all(&reached, None);
            inner.rebuild_overlay();
            inner.changed();
            inner.notify_all();
            return true;
        };
        inner.stats.incremental_syncs = inner.stats.incremental_syncs.saturating_add(1);
        // The record's new changes are on the mirror; what an origin derives is written onto
        // them again, as the rollback took it out.
        inner.reapply_derived(chunk);
        for doc in &changed.nested {
            inner.nested.forget(doc);
        }
        if changed.structure || changed.unplaced {
            inner.rescan();
        }
        let reached = if changed.unplaced || changed.layout {
            everything(&before, &inner.index)
        } else {
            Reached {
                docs: changed.docs.clone(),
                placed: changed.placed.clone(),
            }
        };
        let (moved, wholes, notified) = inner.derive_all(&reached, Some((&from, &ends, &placed)));
        // The overlay was the old transaction's, and what an origin derives was written onto
        // the mirror again: derive it over the indexes as the record's changes left them, so a
        // later write into one of its collections sees the items it holds, at their places.
        inner.rebuild_overlay();
        for (key, ids) in moved {
            items.entry(key).or_default().extend(ids);
        }
        let makers = changed.by;
        let docs = named(changed.docs, items.keys(), wholes, notified, &ends);
        if changed.unplaced || changed.layout {
            inner.changed();
        } else {
            inner.changed_within(&docs);
        }
        let mut listings = inner.splices_since(&old_snapshots, &old_overlay, &items);
        let mut collections = collections_by_doc(items.keys());
        if changed.unplaced {
            inner.notify_all();
        } else {
            for (doc, fields) in docs {
                let listings = listings.remove(&doc).unwrap_or_default();
                let collections = collections.remove(&doc).unwrap_or_default();
                inner.notify(&ChangeSummary {
                    doc: doc.clone(),
                    fields,
                    collections,
                    listings,
                    // Who the record's change names as its maker: a window tells its own
                    // person's change by it, and keeps the rows it changed where they stand.
                    by: makers.get(&doc).cloned().flatten(),
                });
            }
        }
        true
    }
}

impl Inner {
    /// Apply the changes of `source` since the mirror's heads to the mirror of `chunk`, when it
    /// holds no change of its own: what they changed, the versions they made of each resource
    /// they changed ([`idx::reflect::ends_of`]), and where they placed the items they did.
    /// `None` when the mirror must be copied anew.
    fn apply_new_changes(
        &mut self,
        chunk: ChunkNo,
        source: &Doc,
        before: &BTreeMap<DocId, (AppId, TypeId)>,
    ) -> (Option<Changed>, Ends, idx::reflect::Placed) {
        let seen = self.chunks.seen(chunk);
        let caught_up = self
            .chunks
            .mirror(chunk)
            .is_some_and(|mirror| mirror.committed_document().get_heads() == seen);
        if !caught_up {
            return (None, Ends::new(), idx::reflect::Placed::new());
        }
        let changes = source.committed_document().get_changes(seen);
        let applied = self.chunks.mirror_mut(chunk).map(|mirror| {
            let applied = mirror.apply_incoming(changes.iter().cloned());
            write_as_a_known_actor(mirror);
            applied
        });
        // Once the mirror holds them: a change that is no action of a package (an undo) is
        // read for what it wrote, and the versions are the log's, which names the fields each
        // change wrote.
        if let (Some(versions), Some(mirror)) = (self.versions.as_mut(), self.chunks.mirror(chunk))
        {
            note_versions(versions, &changes, mirror, before);
        }
        let what = match (applied, self.chunks.mirror(chunk)) {
            (Some(Ok(())), Some(mirror)) => Some(changed_by(&changes, mirror, before)),
            _ => None,
        };
        let ends = match self.chunks.mirror(chunk) {
            Some(mirror) => idx::reflect::ends_of(&changes, mirror, before),
            None => Ends::new(),
        };
        (what, ends, idx::reflect::placed_by(&changes))
    }
    /// The steps each listed collection took from the indexes `before` (with the overlay
    /// `overlay` over them) to the indexes now, for the items `items`; per document.
    fn splices_since(
        &self,
        before: &BTreeMap<AppId, Arc<idx::Snapshot>>,
        overlay: &Arc<Overlay>,
        items: &ItemTouches,
    ) -> BTreeMap<DocId, Vec<ListingChange>> {
        let mut out: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        for (key, ids) in items {
            let Some((app, _)) = self.index.get(key.doc()) else {
                continue;
            };
            let (Some(old), Some(new)) = (before.get(app), self.view(app)) else {
                continue;
            };
            let old = View {
                snapshot: old,
                overlay,
            };
            if let Ok(splices) = view::splices(&old, &new, key, ids) {
                record_splices(out.entry(key.doc().to_owned()).or_default(), key, splices);
            }
        }
        out
    }
}

/// The documents a sync tells those who read the mirror of, per document: the fields the
/// record's changes named, the collection items that joined, left or moved, the records derived
/// anew whole, the fields that moved on resources the changes did not name, and the versions
/// they made of each resource. What changed, before it is told.
fn named<'a>(
    mut docs: BTreeMap<DocId, Vec<FieldKey>>,
    items: impl Iterator<Item = &'a FieldKey>,
    wholes: BTreeSet<DocId>,
    notified: BTreeMap<DocId, Vec<FieldKey>>,
    ends: &Ends,
) -> BTreeMap<DocId, Vec<FieldKey>> {
    for key in items {
        note_derived(&mut docs, key.doc(), vec![key.clone()]);
    }
    for doc in wholes {
        docs.insert(doc, Vec::new());
    }
    // A field may have moved on a resource the change did not name: what a link target's
    // [`tilolang_runtime::store::LINKS`] reads of what links to it.
    for (doc, keys) in notified {
        note_derived(&mut docs, &doc, keys);
    }
    name_version_changes(ends, &mut docs);
    for fields in docs.values_mut() {
        fields.sort();
        fields.dedup();
    }
    docs
}
