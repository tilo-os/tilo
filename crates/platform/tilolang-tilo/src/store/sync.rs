//! Bringing the store up to date with the workspace record: its new changes applied to the
//! mirror and derived into every index read from, and what they changed told to those who read
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_automerge::ApplyIncoming as _;
use tilolang_runtime::ids::{AppId, DocId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange};

use super::{
    Chunks, Doc, Ends, Inner, ItemTouches, TiloStore, changed_by, collections_by_doc, everything,
    mirror_of, name_version_changes, note_derived, note_versions, record_splices, told_differently,
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
        self.stop_waiting(record);
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
        self.stop_waiting(record);
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

    /// A chunk the store asked its source for and the source could not give is waited on
    /// ([`Inner::waited`]): one the record now holds arrived through the sync, and whatever reads
    /// the record reads it from here on. Nothing else about the chunk is done here: what reads it
    /// reads it as any chunk the store was not sent ([`TiloStore::load_short`]).
    fn stop_waiting(&self, record: &Chunks) {
        if self
            .held()
            .waited
            .iter()
            .all(|chunk| record.mirror(*chunk).is_none())
        {
            return;
        }
        self.changing()
            .waited
            .retain(|chunk| record.mirror(*chunk).is_none());
    }

    fn sync_document(&self, chunk: ChunkNo, doc: &Doc) -> bool {
        let mut inner = self.mutate();
        if inner.chunks.mirror(chunk).is_none() {
            let moved = inner.attach_document(chunk, doc);
            drop(inner);
            if moved {
                self.changing().notify_all();
            }
            return moved;
        }
        let heads = doc.committed_document().get_heads();
        if inner.chunks.seen(chunk) == heads.as_slice() {
            return false;
        }
        // What the splices below are measured from is the listing as those who read the mirror
        // had it before this change: the store's own writes are the mirror's open transaction,
        // which every read holds, so an item one of them placed was already read at its place
        // and the change that brings it takes no step for it. A reader that did not read since
        // is told the collection and reads its items there.
        let before = Before {
            from: inner.chunks.heads(),
            snapshots: inner.answering(),
            overlay: std::mem::take(&mut inner.overlay),
        };
        let Some(mut applied) = inner.take_in(chunk, doc, heads) else {
            // The mirror held a change of its own, which only a copy of the record removes.
            inner.copy_record(chunk, doc);
            return true;
        };
        let derived = inner.derive(&mut applied, &before);
        inner.tell(applied, derived, &before);
        true
    }
}

/// The record's changes a sync applied to the mirror of one chunk, between that and telling those
/// who read it: what they changed, the versions they made of each resource they changed, where
/// they placed the items they did, and the store's own writes the record took, with the items
/// those writes moved.
struct Applied {
    changed: Changed,
    ends: Ends,
    placed: idx::reflect::Placed,
    items: ItemTouches,
    /// Whether those who read the mirror were told every one of the record's changes already, as
    /// the store's own write it made.
    told: bool,
    versions: Ends,
}

/// What those who read the mirror had before a sync: the heads the record moved on from, each
/// answering index's snapshot, and the overlay over them, which held the store's own writes as
/// its readers did. What the splices a sync tells are measured from.
struct Before {
    from: chunk::Stamps,
    snapshots: BTreeMap<AppId, Arc<idx::Snapshot>>,
    overlay: Arc<Overlay>,
}

/// What a sync derived and has yet to tell: the records a change derived anew whole, and the
/// fields that moved on records it did not name.
struct Derived {
    wholes: BTreeSet<DocId>,
    notified: BTreeMap<DocId, Vec<FieldKey>>,
}

impl Inner {
    /// Take a copy of the record `doc` as the mirror of `chunk`, and tell every reader.
    fn copy_record(&mut self, chunk: ChunkNo, doc: &Doc) {
        let before = self.index.clone();
        self.versions.remove(&chunk);
        self.nested.clear();
        self.stats.full_syncs = self.stats.full_syncs.saturating_add(1);
        self.chunks.insert(chunk, mirror_of(doc));
        if chunk == chunk::FIRST {
            self.chunks.reread_placement();
        }
        self.rescan();
        // What the record did not take is not the record's: it is written onto the fresh copy,
        // and derived again over the indexes the record's changes left, so reads keep the values
        // and items it holds where they are.
        self.reapply_kept(chunk);
        let reached = everything(&before, &self.index);
        self.derive_all(&reached, None);
        self.rebuild_overlay();
        self.changed();
        self.notify_all();
    }

    /// Take the record's new changes into the mirror of `chunk`, once its heads have moved: the
    /// store's own writes the record holds leave the mirror and the overlay, the changes are
    /// applied to the mirror, and the store's writes the record has not taken are written onto
    /// them again as the rollback took them out. `None` when the mirror holds a change of its own,
    /// which only a copy of the record removes.
    fn take_in(
        &mut self,
        chunk: ChunkNo,
        doc: &Doc,
        heads: Vec<automerge::ChangeHash>,
    ) -> Option<Applied> {
        // The record's new changes, read once: what the store's writes are the record's by, and
        // what the mirror takes in. Every write they do not hold stays — a change that came from
        // elsewhere leaves the edits a person has here — and is written onto the mirror again
        // below, as the rollback took the whole open transaction out.
        let arrived = doc
            .committed_document()
            .get_changes(self.chunks.seen(chunk));
        let (committed, every_one_an_action) = committed_edits(&arrived);
        let taken = self.drop_committed_local(chunk, &committed);
        self.rollback(chunk);
        let (changed, ends, placed) = self.apply_new_changes(chunk, &arrived);
        self.chunks.set_seen(chunk, heads);
        let changed = changed?;
        self.stats.incremental_syncs = self.stats.incremental_syncs.saturating_add(1);
        if chunk == chunk::FIRST {
            // The record's manifest is where every other chunk's resources are placed: the
            // record's changes may have started a run in a chunk, and a read or a write there
            // routes by what it says now.
            self.chunks.reread_placement();
        }
        // The record's new changes are on the mirror; what the store still holds is written onto
        // them again, as the rollback took it out.
        self.reapply_kept(chunk);
        for doc in &changed.nested {
            self.nested.forget(doc);
        }
        Some(Applied {
            changed,
            ends,
            placed,
            items: taken.items,
            told: taken.told && every_one_an_action,
            versions: taken.versions,
        })
    }

    /// Derive the record's changes the mirror of `chunk` now holds into every index read from:
    /// what they moved of the items a listing holds joins `applied.items`, and the records
    /// derived anew whole and the fields that moved on records the changes did not name are
    /// returned, to be told.
    fn derive(&mut self, applied: &mut Applied, before: &Before) -> Derived {
        // What the index held before the change, when every resource it held and holds now is
        // derived again: a change that writes fields of resources leaves it uncopied.
        let held = (applied.changed.unplaced || applied.changed.layout).then(|| self.index.clone());
        if applied.changed.structure || applied.changed.unplaced {
            self.rescan();
        }
        let reached = held.map_or_else(
            || Reached {
                docs: applied.changed.docs.clone(),
                placed: applied.changed.placed.clone(),
            },
            |held| everything(&held, &self.index),
        );
        let (moved, wholes, notified) = self.derive_all(
            &reached,
            Some((&before.from, &applied.ends, &applied.placed)),
        );
        // A write routed to a chunk above the record whose document this sync does not carry —
        // a record kept as one document that the store also routes by its manifest, or a caller
        // handing over the record alone ([`TiloStore::sync_from`]) — stays in `local`; the
        // change above tells the versions it made, and the index holds them. Keep deriving that
        // write without them ([`Self::settle_versions`]).
        self.settle_versions(&applied.ends);
        // The overlay was the old transaction's, and what an origin derives was written onto
        // the mirror again: derive it over the indexes as the record's changes left them, so a
        // later write into one of its collections sees the items it holds, at their places.
        self.rebuild_overlay();
        for (key, ids) in moved {
            applied.items.entry(key).or_default().extend(ids);
        }
        Derived { wholes, notified }
    }

    /// Tell those who read the mirror what a sync changed: per document the fields the record's
    /// changes named, how its listings moved and which collections' items did, and the records
    /// derived anew whole — or, when the record's changes are the store's own writes and nothing
    /// else, only what reads differently: the versions, which name the change that made them.
    fn tell(&mut self, applied: Applied, derived: Derived, before: &Before) {
        let Derived { wholes, notified } = derived;
        // Those who read the mirror were told what each of the store's own writes changed when
        // its action committed: the mirror and the overlay showed it as the record now has it.
        let told = applied.told
            && wholes.is_empty()
            && !applied.changed.unplaced
            && !applied.changed.layout;
        let docs = named(
            applied.changed.docs,
            applied.items.keys(),
            wholes,
            notified,
            &applied.ends,
        );
        if applied.changed.unplaced || applied.changed.layout {
            self.changed();
        } else {
            self.changed_within(&docs, &applied.items);
        }
        let (docs, listings, collections) = if told {
            (
                told_differently(&applied.ends, &applied.versions),
                BTreeMap::new(),
                BTreeMap::new(),
            )
        } else {
            (
                docs,
                self.splices_since(&before.snapshots, &before.overlay, &applied.items),
                collections_by_doc(applied.items.keys()),
            )
        };
        if applied.changed.unplaced {
            self.notify_all();
        } else {
            self.tell_synced(docs, (listings, collections), &applied.changed.by);
        }
    }

    /// The snapshot each index that answers reads now, by its app.
    fn answering(&self) -> BTreeMap<AppId, Arc<idx::Snapshot>> {
        self.apps
            .iter()
            .filter(|(_, held)| held.answers())
            .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
            .collect()
    }

    /// Tell those who read the mirror what a sync changed: per document, the fields `docs` names,
    /// with how its collections' listings moved and which collections' items did, and who the
    /// record's change names as its maker (`makers`): a window tells its own person's change by
    /// it, and keeps the rows it changed where they stand.
    fn tell_synced(
        &mut self,
        docs: BTreeMap<DocId, Vec<FieldKey>>,
        (mut listings, mut collections): (
            BTreeMap<DocId, Vec<ListingChange>>,
            BTreeMap<DocId, Vec<FieldKey>>,
        ),
        makers: &crate::reads::By,
    ) {
        for (doc, fields) in docs {
            self.notify(&ChangeSummary {
                listings: listings.remove(&doc).unwrap_or_default(),
                collections: collections.remove(&doc).unwrap_or_default(),
                by: makers.get(&doc).cloned().flatten(),
                doc,
                fields,
            });
        }
    }

    /// Apply `changes`, the record's changes since the mirror's heads, to the mirror of `chunk`,
    /// when it holds no change of its own: what they changed, the versions they made of each
    /// resource they changed ([`idx::reflect::ends_of`]), and where they placed the items they
    /// did. `None` when the mirror must be copied anew.
    fn apply_new_changes(
        &mut self,
        chunk: ChunkNo,
        changes: &[automerge::Change],
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
            note_versions(versions, changes, mirror, &self.index);
        }
        let what = match (applied, self.chunks.mirror(chunk)) {
            (Some(Ok(())), Some(mirror)) => Some(changed_by(changes, mirror, &self.index)),
            _ => None,
        };
        let ends = match self.chunks.mirror(chunk) {
            Some(mirror) => idx::reflect::ends_of(changes, mirror, &self.index),
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
/// action records the action it made, and so the edits it wrote, whole or with its large parts in
/// the record's content store, where this device's own are always kept; a change that records
/// none — the record's own genesis, a merge — took none of the store's writes. Also whether every
/// one of `changes` records some.
pub(super) fn committed_edits(
    changes: &[automerge::Change],
) -> (Vec<(automerge::ChangeHash, DynEdit)>, bool) {
    let mut every = true;
    let mut out = Vec::new();
    for change in changes {
        let hash = change.hash();
        let edits = idx::reflect::edits_of(change);
        every &= !edits.is_empty();
        out.extend(edits.into_iter().map(|edit| (hash, edit)));
    }
    (out, every)
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
