//! Bringing the store up to date with the workspace record: its new changes applied to the
//! mirror and derived into every index read from, and what they changed told to those who read
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_actions::Described;
use tilo_automerge::ApplyIncoming as _;
use tilolang_runtime::ids::{AppId, DocId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange};

use super::{
    Chunks, Doc, Ends, Inner, ItemTouches, LocalWrite, TiloStore, changed_by, collections_by_doc,
    everything, name_version_changes, note_derived, note_versions, record_splices,
    told_differently, write_as_a_known_actor,
};
use crate::action::{DynAction, DynEdit};
use crate::chunk::{self, ChunkNo};
use crate::index::derive::Reached;
use crate::index::overlay::Overlay;
use crate::index::view::{self, View};
use crate::index::{self as idx};
use crate::reads::Changed;

impl TiloStore {
    /// Take in what the copies `record` of the workspace record's chunks, which their caller
    /// holds and writes, gained since the store last read them: each chunk's new changes are
    /// merged into the store's own document of it and derived as any write of the record's
    /// holder is ([`Self::write_chunk`]). The chunks above the record go first: what they hold
    /// is already theirs when the record's own change derives over it. Whether anything moved.
    pub fn sync(&self, record: &Chunks) -> bool {
        let mut moved = false;
        for (chunk, doc) in record.docs() {
            if chunk != chunk::FIRST {
                moved |= self.merge(chunk, doc);
            }
        }
        if let Some(record) = record.first() {
            moved |= self.merge(chunk::FIRST, record);
        }
        moved
    }

    /// [`Self::sync`] of a record the caller holds as one document, chunk [`chunk::FIRST`].
    pub fn sync_from(&self, record: &Doc) -> bool {
        self.merge(chunk::FIRST, record)
    }

    /// Merge what `doc`, a copy of chunk `chunk` its caller writes, holds that the store's
    /// document of it lacks. Whether anything moved.
    fn merge(&self, chunk: ChunkNo, doc: &Doc) -> bool {
        let theirs = doc.committed_document();
        let arrived = {
            let inner = self.held();
            let Some(mine) = inner.chunks.held(chunk) else {
                drop(inner);
                return self
                    .write_chunk(chunk, |mine| take(mine, theirs.get_changes(&[])))
                    .1;
            };
            let mine = mine.committed_document();
            let have = mine.get_heads();
            if have == theirs.get_heads() {
                return false;
            }
            if have
                .iter()
                .all(|head| theirs.get_change_meta_by_hash(head).is_some())
            {
                theirs.get_changes(&have)
            } else {
                theirs.get_changes_added(mine)
            }
        };
        if arrived.is_empty() {
            return false;
        }
        self.write_chunk(chunk, |mine| take(mine, arrived)).1
    }

    /// Hold the record's chunks `chunks` from here on, the one copy of each: the record's own is
    /// read now, and every chunk above it when something reads or writes into it
    /// ([`Self::load_chunk`]). Its holder reads and writes them through the store
    /// ([`Self::with_record`], [`Self::write_record`]).
    pub fn adopt(&self, mut chunks: Chunks) {
        chunks.park_above_first();
        let first = chunks.take_attached(chunk::FIRST);
        let mut inner = self.mutate();
        inner.adopted = true;
        inner.waited.clear();
        inner.chunks = chunks;
        let moved = match first {
            Some(first) => inner.attach_document(chunk::FIRST, first),
            None => false,
        };
        drop(inner);
        if moved {
            self.changing().notify_all();
        }
    }

    /// Hand every chunk the store holds back, its unsaved writes out of them: what a host
    /// opened anew on another build takes over ([`Self::adopt`]).
    pub fn release(&self) -> Chunks {
        let mut inner = self.mutate();
        let held: Vec<ChunkNo> = inner.chunks.numbers().collect();
        for chunk in held {
            inner.rollback(chunk);
        }
        inner.adopted = false;
        std::mem::take(&mut inner.chunks)
    }

    /// Read the record's chunks as their holder has them: the committed changes, and the
    /// store's unsaved writes as each one's open transaction.
    pub fn with_record<R>(&self, read: impl FnOnce(&Chunks) -> R) -> R {
        read(&self.held().chunks)
    }

    /// Write the record's chunk `chunk`, as its holder does: the store's unsaved writes are
    /// taken out of it first, so the write meets the committed changes alone and never commits
    /// them; then the chunk's new changes are derived into every index read from and told, with
    /// nothing applied again (the document holds them), and the unsaved writes they did not
    /// take are written back. A chunk the store has not read yet is written as it is. Also
    /// whether anything moved.
    pub fn write_chunk<R>(&self, chunk: ChunkNo, write: impl FnOnce(&mut Doc) -> R) -> (R, bool) {
        let out = {
            let mut inner = self.mutate();
            inner.rollback(chunk);
            write(inner.chunks.open_held(chunk))
        };
        let read = self.held().chunks.mirror(chunk).is_some();
        let moved = read && self.catch_up(chunk);
        (out, moved)
    }

    /// Have the store write an action's writes as `author` (the record's holder) where they are
    /// the first open writes of their chunk, so they are the action's change as written
    /// ([`Self::commit_open`]).
    pub fn set_author(&self, author: &[u8]) {
        self.changing().author = Some(automerge::ActorId::from(author));
    }

    /// Run `run` with the store writing as `author`: an invocation the door builds for an agent
    /// is the agent's change, written as theirs.
    pub fn writing_as<R>(&self, author: &[u8], run: impl FnOnce() -> R) -> R {
        let held = self
            .changing()
            .author
            .replace(automerge::ActorId::from(author));
        let out = run();
        self.changing().author = held;
        out
    }

    /// Whether the store's open writes into chunk `chunk` begin with the action `part`: each of
    /// its edits in its order, as one action of the store's, with the ids replaying it mints.
    /// Then they are its change as replaying it writes it, to commit ([`Self::commit_open`]),
    /// with what it did as history names it, read before its first write.
    #[must_use]
    pub fn written_as(&self, chunk: ChunkNo, part: &DynAction) -> Option<Described> {
        self.held().written_as(chunk, part)
    }

    /// Commit the writes of the action `part` into chunk `chunk`, which [`Self::written_as`]
    /// found, as `author`'s change with `commit`: the action is written once. When the chunk
    /// holds other open writes as well (derived ones, which no change carries, or an action's
    /// queued after it), or they were written under another actor, the part is written alone
    /// under `author` first, and the rest goes back on after the commit. Its new change is
    /// derived as [`Self::write_chunk`] derives one; also whether anything moved.
    pub fn commit_open<R>(
        &self,
        chunk: ChunkNo,
        part: &DynAction,
        author: &[u8],
        commit: impl FnOnce(&mut Doc) -> R,
    ) -> (R, bool) {
        let out = {
            let mut inner = self.mutate();
            inner.stats.written_once = inner.stats.written_once.saturating_add(1);
            inner.write_alone(chunk, part.edits.len(), author);
            commit(inner.chunks.open_held(chunk))
        };
        let moved = self.catch_up(chunk);
        (out, moved)
    }

    /// Derive what the store's own document of `chunk` gained since the store last read it:
    /// with nothing to apply: the document holds the changes.
    fn catch_up(&self, chunk: ChunkNo) -> bool {
        let mut inner = self.mutate();
        if !inner.chunks.is_seen(chunk) {
            // A chunk the holder started by writing into it: read as any chunk is attached.
            let Some(doc) = inner.chunks.take_attached(chunk) else {
                return false;
            };
            let moved = inner.attach_document(chunk, doc);
            drop(inner);
            if moved {
                self.changing().notify_all();
            }
            return moved;
        }
        let Some(doc) = inner.chunks.mirror(chunk) else {
            return false;
        };
        let heads = doc.committed_document().get_heads();
        let seen = inner.chunks.seen(chunk);
        if seen == heads.as_slice() {
            if let Some(mirror) = inner.chunks.mirror_mut(chunk) {
                write_as_a_known_actor(mirror);
            }
            inner.reapply_kept(chunk);
            return false;
        }
        let kept = seen.iter().all(|head| {
            doc.committed_document()
                .get_change_meta_by_hash(head)
                .is_some()
        });
        if !kept {
            // The holder took changes back (an action the workspace refused partway): what the
            // store derived of them goes, and the chunk is read again.
            inner.chunks.set_seen(chunk, heads);
            inner.reread(chunk);
            return true;
        }
        let arrived = doc.committed_document().get_changes(seen);
        // The record's own document holds its new changes already: the chunk stood at what the
        // store saw of it, and every index that took that in takes in the new changes alone.
        let mut from = inner.chunks.heads();
        from.insert(chunk, seen.to_vec());
        let before = Before {
            from,
            snapshots: inner.answering(),
            overlay: std::mem::take(&mut inner.overlay),
        };
        let Some(mut applied) = inner.take_in(chunk, &arrived, heads) else {
            inner.reread(chunk);
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
    /// Whether the open writes of chunk `chunk` begin with the action `part`, written as
    /// replaying it writes ([`TiloStore::written_as`]); with what it did, read before its first
    /// write. `None` when they do not.
    fn written_as(&self, chunk: ChunkNo, part: &DynAction) -> Option<Described> {
        let mut writes = self
            .local
            .iter()
            .filter(|write| write.chunk == chunk && !write.derived);
        let first = writes.clone().next()?;
        let mut subject = None;
        for (index, edit) in part.edits.iter().enumerate() {
            let write = writes.next()?;
            let same = write.action == first.action
                && write.index == index
                && write.edit.bare() == edit.bare()
                && write.app.as_str() == written_under(edit, &part.app);
            if !same {
                return None;
            }
            subject = subject.or(write.subject.as_ref());
        }
        if writes
            .next()
            .is_some_and(|next| next.action == first.action)
        {
            return None;
        }
        // The part's subject is read for the app the action is of, as replaying it reads it.
        let subject = match subject {
            Some((read, subject)) if read.as_str() == part.app => subject.clone(),
            Some(_) => return None,
            None => None,
        };
        Some(part.described_with(|_| subject))
    }

    /// Leave the first `count` saved writes of chunk `chunk` (an action's, [`Self::written_as`])
    /// as its open transaction alone, written under `author`: as they are when they are all it
    /// holds and were written so, else written again so. What else it held goes back on once
    /// the action's change is taken in ([`Self::reapply_kept`]).
    fn write_alone(&mut self, chunk: ChunkNo, count: usize, author: &[u8]) {
        let open = self
            .local
            .iter()
            .filter(|write| write.chunk == chunk)
            .count();
        let as_author = self
            .chunks
            .mirror(chunk)
            .is_some_and(|doc| doc.get_actor().to_bytes() == author);
        if open == count && as_author {
            return;
        }
        let part: Vec<LocalWrite> = self
            .local
            .iter()
            .filter(|write| write.chunk == chunk && !write.derived)
            .take(count)
            .cloned()
            .collect();
        self.rollback(chunk);
        let Some(doc) = self.chunks.mirror_mut(chunk) else {
            return;
        };
        doc.set_actor(automerge::ActorId::from(author));
        for write in &part {
            let seed = crate::action::edit_seed(write.index);
            let written = crate::action::apply_edits(
                doc,
                &write.app,
                std::slice::from_ref(&write.edit),
                &seed,
            );
            if let Err(error) = written {
                tracing::error!(app = %write.app, %error, "an action's write did not apply again");
            }
        }
    }

    /// Read the store's document of `chunk` again whole, and tell every reader: after it lost
    /// changes the store had derived.
    fn reread(&mut self, chunk: ChunkNo) {
        let before = self.index.clone();
        self.versions.remove(&chunk);
        self.nested.clear();
        self.stats.full_syncs = self.stats.full_syncs.saturating_add(1);
        if let Some(mirror) = self.chunks.mirror_mut(chunk) {
            write_as_a_known_actor(mirror);
        }
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
        arrived: &[automerge::Change],
        heads: Vec<automerge::ChangeHash>,
    ) -> Option<Applied> {
        // The record's new changes, read once: what the store's writes are the record's by, and
        // what the mirror takes in (`held`: the mirror is the record's own document, and holds
        // them already). Every write they do not hold stays — a change that came from elsewhere
        // leaves the edits a person has here — and is written onto the mirror again below, as
        // the rollback took the whole open transaction out.
        let (committed, every_one_an_action) = committed_edits(arrived);
        let taken = self.drop_committed_local(chunk, &committed);
        self.rollback(chunk);
        let (changed, ends, placed) = self.apply_new_changes(chunk, arrived);
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
        let Some(mirror) = self.chunks.mirror_mut(chunk) else {
            return (None, Ends::new(), idx::reflect::Placed::new());
        };
        write_as_a_known_actor(mirror);
        // Once the mirror holds them: a change that is no action of a package (an undo) is
        // read for what it wrote, and the versions are the log's, which names the fields each
        // change wrote.
        if let (Some(versions), Some(mirror)) =
            (self.versions.get_mut(&chunk), self.chunks.mirror(chunk))
        {
            note_versions(versions, changes, mirror, &self.index);
        }
        let what = self
            .chunks
            .mirror(chunk)
            .map(|mirror| changed_by(changes, mirror, &self.index));
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

/// The app `edit` of an action of `app` is written under: the one it is addressed to, else the
/// action's.
fn written_under<'a>(mut edit: &'a DynEdit, mut app: &'a str) -> &'a str {
    loop {
        match edit {
            DynEdit::In {
                app: within,
                edit: inner,
            } => {
                app = within;
                edit = inner;
            }
            DynEdit::Chunk { edit: inner, .. } => edit = inner,
            _ => return app,
        }
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

/// Apply `changes` to `doc`: the number it gained, as a merge counts them.
fn take(doc: &mut Doc, changes: Vec<automerge::Change>) -> usize {
    let count = changes.len();
    match doc.apply_incoming(changes) {
        Ok(()) => count,
        Err(error) => {
            tracing::error!(%error, "a copy of the record's changes did not apply");
            0
        }
    }
}
