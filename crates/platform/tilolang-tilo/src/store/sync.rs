//! Bringing the store up to date with the workspace record: its new changes applied to the
//! mirror and derived into every index read from, and what they changed told to those who read
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_automerge::ApplyIncoming as _;
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange};

use super::{
    Chunks, Doc, Ends, Inner, ItemTouches, TiloStore, changed_by, collections_by_doc, everything,
    mirror_of, name_version_changes, note_derived, note_versions, record_splices,
    write_as_a_known_actor,
};
use crate::action::DynEdit;
use crate::chunk::{self, ChunkNo};
use crate::index::derive::Reached;
use crate::index::overlay::Overlay;
use crate::index::view::{self, View};
use crate::index::{self as idx};
use crate::reads::Changed;

impl TiloStore {
    /// Bring the store up to date with the workspace record: when a chunk's heads moved, the
    /// store's own writes the record's new changes hold are dropped from the mirror and the
    /// overlay — every write they do not hold stays, as the record has not taken it — the chunk's
    /// new changes are applied to the mirror and derived into each index, and each subscriber is
    /// told which fields of which resources changed. A store without the record takes its own
    /// chunk from it, and catches each index up from its heads. Returns whether any chunk had
    /// moved.
    ///
    /// Every chunk the record holds is taken in, so a write the store routed to any of them
    /// settles as one routed to the record's own: the chunk's change is in the set the sync
    /// applies, and its write leaves the store with it. The chunks above the record go first:
    /// what they hold is already theirs when the record's own change derives over it.
    pub fn sync(&self, record: &Chunks) -> bool {
        let mut moved = false;
        for (chunk, doc) in record.docs() {
            if chunk != chunk::FIRST {
                moved |= self.sync_document(chunk, doc);
            }
        }
        if let Some(record) = record.first() {
            moved |= self.sync_document(chunk::FIRST, record);
        }
        moved
    }

    /// [`Self::sync`] of the chunks the store already holds of `record`: a chunk it was not
    /// sent is left out, and is read from the chunk loader when a read or a write reaches it
    /// ([`TiloStore::set_chunk_loader`]). Returns whether any chunk had moved.
    pub fn sync_held(&self, record: &Chunks) -> bool {
        let held: Vec<ChunkNo> = self.held().chunks.numbers().collect();
        let mut moved = false;
        for chunk in held.into_iter().filter(|chunk| *chunk != chunk::FIRST) {
            if let Some(doc) = record.mirror(chunk) {
                moved |= self.sync_document(chunk, doc);
            }
        }
        if self.held().chunks.has_record()
            && let Some(doc) = record.first()
        {
            moved |= self.sync_document(chunk::FIRST, doc);
        }
        moved
    }

    /// [`Self::sync`] of a record the caller hands over as one document: the store takes it as
    /// chunk [`chunk::FIRST`]. A record whose manifest names chunks above it is synced through
    /// [`Self::sync`] instead, which takes those chunks in too; a write the store routed to one
    /// of them stays pending here until its chunk is had. Returns whether it had moved.
    pub fn sync_from(&self, record: &Doc) -> bool {
        self.sync_document(chunk::FIRST, record)
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
        let old_snapshots: BTreeMap<AppId, Arc<idx::Snapshot>> = inner
            .apps
            .iter()
            .filter(|(_, held)| held.answers())
            .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
            .collect();
        // What the splices below are measured from is the listing as those who read the mirror
        // had it before this change: the store's own writes are the mirror's open transaction,
        // which every read holds, so an item one of them placed was already read at its place
        // and the change that brings it takes no step for it. A reader that did not read since
        // is told the collection and reads its items there.
        let old_overlay = std::mem::take(&mut inner.overlay);
        // The store's own writes the record's new changes hold leave the mirror: the items they
        // added, removed or moved are moved again below, as the record has them. Every write
        // they do not hold stays — a change that came from elsewhere leaves the edits a person
        // has here — and is written onto the record's new copy again below, as the rollback took
        // the whole open transaction out.
        // The record's new changes, read once: what the store's writes are the record's by, and
        // what the mirror takes in.
        let arrived = doc
            .committed_document()
            .get_changes(inner.chunks.seen(chunk));
        let committed = committed_edits(&arrived);
        let mut items = inner.drop_committed_local(chunk, &committed);
        inner.rollback(chunk);
        let (changed, ends, placed) = inner.apply_new_changes(chunk, &arrived, &before);
        inner.chunks.set_seen(chunk, heads);
        let Some(changed) = changed else {
            // The mirror held a change of its own, which only a copy of the record removes.
            inner.versions.remove(&chunk);
            inner.nested.clear();
            inner.stats.full_syncs = inner.stats.full_syncs.saturating_add(1);
            inner.chunks.insert(chunk, mirror_of(doc));
            if chunk == chunk::FIRST {
                inner.chunks.reread_placement();
            }
            inner.rescan();
            // What the record did not take is not the record's: it is written onto the fresh
            // copy, and derived again over the indexes the record's changes left, so reads keep
            // the values and items it holds where they are.
            inner.reapply_kept(chunk);
            let reached = everything(&before, &inner.index);
            inner.derive_all(&reached, None);
            inner.rebuild_overlay();
            inner.changed();
            inner.notify_all();
            return true;
        };
        inner.stats.incremental_syncs = inner.stats.incremental_syncs.saturating_add(1);
        if chunk == chunk::FIRST {
            // The record's manifest is where every other chunk's resources are placed: the
            // record's changes may have started a run in a chunk, and a read or a write there
            // routes by what it says now.
            inner.chunks.reread_placement();
        }
        // The record's new changes are on the mirror; what the store still holds is written onto
        // them again, as the rollback took it out.
        inner.reapply_kept(chunk);
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
        // A write routed to a chunk above the record whose document this sync does not carry —
        // a record kept as one document that the store also routes by its manifest, or a caller
        // handing over the record alone ([`Self::sync_from`]) — stays in `local`; the change
        // above tells the versions it made, and the index holds them. Keep deriving that write
        // without them ([`Inner::settle_versions`]).
        inner.settle_versions(&ends);
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
            inner.changed_within(&docs, &items);
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
    /// Apply `changes`, the record's changes since the mirror's heads, to the mirror of `chunk`,
    /// when it holds no change of its own: what they changed, the versions they made of each
    /// resource they changed ([`idx::reflect::ends_of`]), and where they placed the items they
    /// did. `None` when the mirror must be copied anew.
    fn apply_new_changes(
        &mut self,
        chunk: ChunkNo,
        changes: &[automerge::Change],
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
        let applied = self.chunks.mirror_mut(chunk).map(|mirror| {
            let applied = mirror.apply_incoming(changes.iter().cloned());
            write_as_a_known_actor(mirror);
            applied
        });
        // Once the mirror holds them: a change that is no action of a package (an undo) is
        // read for what it wrote, and the versions are the log's, which names the fields each
        // change wrote.
        if let (Some(versions), Some(mirror)) =
            (self.versions.get_mut(&chunk), self.chunks.mirror(chunk))
        {
            note_versions(versions, changes, mirror, before);
        }
        let what = match (applied, self.chunks.mirror(chunk)) {
            (Some(Ok(())), Some(mirror)) => Some(changed_by(changes, mirror, before)),
            _ => None,
        };
        let ends = match self.chunks.mirror(chunk) {
            Some(mirror) => idx::reflect::ends_of(changes, mirror, before),
            None => Ends::new(),
        };
        (what, ends, idx::reflect::placed_by(changes))
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
                clock: self.clock.read(),
            };
            if let Ok(splices) = view::splices(&old, &new, key, ids) {
                record_splices(out.entry(key.doc().to_owned()).or_default(), key, splices);
            }
        }
        out
    }
}

/// The edits `changes` carry, in order, each with the change carrying it: what the store's own
/// writes are the record's by ([`Inner::drop_committed_local`]). Every change of a tilolang
/// action records the action it made, and so the edits it wrote; a change that records none —
/// the record's own genesis, a merge — took none of the store's writes.
fn committed_edits(changes: &[automerge::Change]) -> Vec<(automerge::ChangeHash, DynEdit)> {
    changes
        .iter()
        .flat_map(|change| {
            let hash = change.hash();
            idx::reflect::edits_of(change)
                .into_iter()
                .map(move |edit| (hash, edit))
        })
        .collect()
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
