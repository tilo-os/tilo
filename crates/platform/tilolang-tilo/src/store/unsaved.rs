//! The store's **unsaved layer**: what an origin derives, which the workspace record never holds,
//! and the writes the store replays when it puts the mirror back (rollback, rewind).
//!
//! A type whose origin kind is derived (`type Desk (origin: session.desk)`,
//! [`crate::schema::TypeSchema::derived`]) is written into the same mirror open transaction as
//! any write — so reads see it at once — but committing it queues nothing ([`commit_derived`]).
//! It stays in the mirror's open transaction and the overlay, which is what reads answer from.
//!
//! The record moving on ([`crate::store::TiloStore::sync`]) drops the store's writes the record
//! now holds and leaves every other one — the derived ones, which the record never holds, and
//! the saved ones whose change has not come back yet: [`Inner::drop_committed_local`] takes the
//! committed ones out of [`super::Inner::local`] — the overlay keeps them until the index their
//! app reads from reflects the change that carried them — [`Inner::rescan`] keeps the derived docs
//! known, and [`Inner::reapply_kept`] writes everything still pending onto the record's new copy
//! again. An origin's update thus never round-trips through the record: no action is committed,
//! no peer replays it, and no index is rebuilt for it; and a change that arrives from elsewhere
//! leaves the edits a person has here where they are.
//!
//! [`Inner::reapply`] is the plain replay of everything the store wrote, which rollback and
//! rewind use; the derived writes it leaves in place are the unsaved layer's.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use automerge::ChangeHash;
use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ChangeNote, ChangeSummary, FieldKey, ListingChange, Version};

use crate::action::{DynEdit, Made, apply_edits};
use crate::chunk::{ChunkNo, Chunks, Stamps};
use crate::index as idx;
use crate::index::derive::Derive;
use crate::index::overlay::{Overlay, OverlaySink};
use crate::index::reflect::Ends;
use crate::schema::lookup;

use super::{Accepted, Inner, ItemTouches, LocalWrite, TxnState, scan_of, version_fields};

/// Whether a write touches only resources its origin derives
/// ([`crate::schema::TypeSchema::derived`]): it belongs in the unsaved layer. `touched` names
/// the fields it writes, `items` the collection items it adds, removes or moves; a write that
/// names no field (an outside effect queued on a resource) is classified by `resource`, the
/// document its edit addresses.
pub(super) fn writes_derived(
    inner: &Inner,
    resource: &str,
    touched: &[FieldKey],
    items: &[(FieldKey, String)],
) -> bool {
    let docs: Vec<&str> = touched
        .iter()
        .map(FieldKey::doc)
        .chain(items.iter().map(|(key, _)| key.doc()))
        .chain(
            (touched.is_empty() && items.is_empty())
                .then_some(tilolang_runtime::store::doc_of(resource)),
        )
        .collect();
    !docs.is_empty()
        && docs.iter().all(|doc| {
            inner
                .index
                .get(*doc)
                .is_some_and(|(_, ty)| lookup(ty).is_some_and(crate::schema::TypeSchema::derived))
        })
}

/// Commit a transaction that wrote only what an origin derives: tell each reader what changed,
/// and queue, send and version nothing. Whether it did (a transaction with any saved write is
/// the record's, and the caller commits it as ever).
pub(super) fn commit_derived(inner: &mut Inner, state: &mut TxnState, by: &str) -> bool {
    if state.saved > 0 {
        return false;
    }
    tell(inner, state, by);
    true
}

/// Note and tell what the transaction `state` committed to the record, the store's writes up to
/// `writes` its own: it is told now, as the mirror's open transaction and the overlay show it,
/// and the record's change that brings it back tells only what the record says differently
/// ([`super::TiloStore::sync`]). A `marked` transaction (a migration's or a trigger's mark) wrote
/// what it names no field of, and its change tells all it wrote.
pub(super) fn tell_saved(
    inner: &mut Inner,
    state: &mut TxnState,
    note: &ChangeNote,
    writes: usize,
    marked: bool,
) {
    for doc in state.touched.keys() {
        inner
            .notes
            .entry(doc.clone())
            .or_default()
            .push(note.clone());
    }
    tell(inner, state, &note.invoker);
    if !marked {
        let start = state.start.min(writes);
        for write in inner.local.iter_mut().take(writes).skip(start) {
            write.told = true;
        }
    }
}

/// Tell whoever reads what the transaction `state` wrote: each document it reached, with the
/// fields it changed and how each collection's listing moved, `by` naming who made it. Returns
/// the documents told.
pub(super) fn tell(inner: &mut Inner, state: &mut TxnState, by: &str) -> Vec<DocId> {
    let mut collections = std::mem::take(&mut state.collections);
    let mut told = Vec::new();
    for (doc, fields) in std::mem::take(&mut state.touched) {
        if fields.is_empty() {
            continue;
        }
        let (own, rest): (Vec<ListingChange>, Vec<ListingChange>) =
            std::mem::take(&mut state.listings)
                .into_iter()
                .partition(|change| change.key.doc() == doc);
        state.listings = rest;
        inner.notify(&ChangeSummary {
            doc: doc.clone(),
            fields: fields.into_iter().collect(),
            collections: collections
                .remove(&doc)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            listings: own,
            by: Some(by.to_owned()),
        });
        told.push(doc);
    }
    told
}

/// The store's writes a sync found the record holds ([`Inner::drop_committed_local`]).
pub(super) struct Taken {
    /// The collection items they added, removed or moved.
    pub(super) items: ItemTouches,
    /// Whether they are every edit the record's new changes carry, and those who read the
    /// mirror were told each when its action committed.
    pub(super) told: bool,
    /// The versions they told, provisional: per resource, what its action made of it.
    pub(super) versions: super::Ends,
}

/// Keep what `write` took out of each collection it moved items of, with the version that
/// removed each, as the overlay `sink` holds it: what its lists show struck until the person
/// has seen them go ([`crate::index::REMOVED`]).
pub(super) fn derive_removed(
    derive: &mut Derive<'_>,
    sink: &mut OverlaySink<'_>,
    app: &str,
    ends: &std::collections::BTreeMap<String, Ends>,
    items: &[(FieldKey, String)],
) {
    let mut moved = ItemTouches::new();
    for (key, item) in items {
        moved.entry(key.clone()).or_default().insert(item.clone());
    }
    if let Err(error) = derive.removed_of(sink, app, ends, &moved) {
        tracing::warn!(app, %error, "a removal was not indexed");
    }
}

impl Inner {
    /// Tell whoever reads what `write` reached that it changed.
    pub(super) fn touch(&mut self, write: &LocalWrite) {
        let mut reached: std::collections::BTreeMap<DocId, Vec<FieldKey>> =
            std::collections::BTreeMap::new();
        let mut items: ItemTouches = ItemTouches::new();
        for (key, id) in &write.items {
            reached
                .entry(key.doc().to_owned())
                .or_default()
                .push(key.clone());
            items.entry(key.clone()).or_default().insert(id.clone());
        }
        for key in &write.touched {
            reached
                .entry(key.doc().to_owned())
                .or_default()
                .push(key.clone());
        }
        self.changed_within(&reached, &items);
    }

    /// Derive one write into the overlay, naming what it derived to whoever reads: a field may
    /// move on a resource the write did not name.
    pub(super) fn derive_write(&mut self, write: &mut LocalWrite) {
        if !self.chunks.has_record() {
            return;
        }
        let Some(held) = self.apps.get(&write.app).filter(|held| held.answers()) else {
            return;
        };
        let snapshot = std::sync::Arc::clone(&held.snapshot);
        let overlay = std::sync::Arc::make_mut(&mut self.overlay);
        let mut sink = crate::index::overlay::OverlaySink {
            overlay,
            snapshot: Some(&snapshot),
        };
        let mut derive = crate::index::derive::Derive::new(
            &self.chunks,
            &self.index,
            &self.nested,
            self.clock.read(),
        );
        if let Err(error) = derive.changes(&mut sink, &write.app, &write.reached()) {
            tracing::warn!(app = write.app, %error, "a change was not indexed");
        }
        for keys in derive.notified.into_values() {
            for key in keys {
                if !write.touched.contains(&key) {
                    write.touched.push(key);
                }
            }
        }
    }

    /// The store's writes the record now holds — the ones in the chunk `chunk` whose edit is one
    /// of `committed`, the edits the record's new changes carry, each with the change carrying
    /// it — taking them out of the store's pending writes. Each stays in the overlay, as
    /// accepted by its change, until the index its app reads from reflects that change
    /// ([`Self::rebuild_overlay`]): an index still being derived off the frame does not hold it
    /// yet, and the write reads back from the overlay meanwhile. Every write the record has not
    /// taken stays pending: a change that arrives from elsewhere leaves the edits a person has
    /// here, and the reads that answer from the store keep showing them, until the record's
    /// change for them comes back.
    ///
    /// Also says whether those who read the mirror were told every one of `committed` already:
    /// each is a write of the store's, told when its action committed ([`tell`]), and with
    /// the versions those writes told provisionally.
    pub(super) fn drop_committed_local(
        &mut self,
        chunk: ChunkNo,
        committed: &[(ChangeHash, DynEdit)],
    ) -> Taken {
        let mut items = ItemTouches::new();
        let mut told = true;
        let mut versions = super::Ends::new();
        let mut held: Vec<(ChangeHash, DynEdit)> = committed
            .iter()
            .map(|(change, edit)| (*change, edit.bare().clone()))
            .collect();
        let mut kept = Vec::with_capacity(self.local.len());
        for write in std::mem::take(&mut self.local) {
            let at = if write.derived || write.chunk != chunk {
                None
            } else {
                let bare = write.edit.bare();
                held.iter().position(|(_, edit)| edit == bare)
            };
            // A write an origin derives is never the record's, whatever edit the record's
            // changes carry: it stays in the mirror and the overlay.
            let Some(at) = at else {
                kept.push(write);
                continue;
            };
            let (change, _) = held.swap_remove(at);
            for (key, id) in &write.items {
                items.entry(key.clone()).or_default().insert(id.clone());
            }
            told &= write.told;
            if let Some(ends) = &write.versions {
                versions.extend(ends.iter().map(|(at, end)| (at.clone(), end.clone())));
            }
            self.accepted.push(Accepted { write, change });
        }
        self.local = kept;
        // An edit the record holds in this chunk's document that the store routed to another
        // chunk — a record kept as one document, whose manifest places items in chunks above it —
        // stays pending there ([`super::TiloStore::sync`]); it was told all the same.
        for (_, edit) in &held {
            let Some(write) = self
                .local
                .iter()
                .find(|write| !write.derived && write.edit.bare() == edit)
            else {
                told = false;
                continue;
            };
            told &= write.told;
            if let Some(ends) = &write.versions {
                versions.extend(ends.iter().map(|(at, end)| (at.clone(), end.clone())));
            }
        }
        Taken {
            items,
            told,
            versions,
        }
    }

    /// Write the store's pending writes of the chunk `chunk` onto the mirror again, as pending
    /// ops: after the record's changes reached it, which rolled the whole open transaction back.
    /// What the record has not taken is still what the store answers, so it is written back as
    /// the store wrote it; the mirror's committed state (the record's) is untouched, so nothing
    /// else rolls back.
    pub(super) fn reapply_kept(&mut self, chunk: ChunkNo) {
        let kept: Vec<LocalWrite> = self
            .local
            .iter()
            .filter(|write| write.chunk == chunk)
            .cloned()
            .collect();
        if kept.is_empty() {
            return;
        }
        if let Some(mirror) = self.chunks.mirror_mut(chunk) {
            for write in &kept {
                if let Err(error) = apply_edits(
                    mirror,
                    &write.app,
                    std::slice::from_ref(&write.edit),
                    &write.seed,
                ) {
                    tracing::error!(app = write.app, %error, "a pending write did not apply again");
                }
            }
        }
    }

    /// Re-read which resources the documents the store holds have: those of each chunk, plus
    /// what an origin derives (those are in the mirrors still, as their open writes, but
    /// [`super::scan_of`] reads only committed records).
    pub(super) fn rescan(&mut self) {
        let derived: Vec<(DocId, (AppId, TypeId))> = self
            .index
            .iter()
            .filter(|(_, (_, ty))| lookup(ty).is_some_and(crate::schema::TypeSchema::derived))
            .map(|(doc, held)| (doc.clone(), held.clone()))
            .collect();
        self.index = scan_of(&self.chunks);
        for (doc, held) in derived {
            self.index.insert(doc, held);
        }
    }

    /// Re-read which resources the chunk `chunk` has, and only those: what [`super::scan`] found
    /// in it now ([`Self::rescan`] reads every document the store holds). What the chunk held
    /// before and does not any more leaves, unless the record's manifest places it in another
    /// chunk, which the store reads when that chunk arrives. An origin's derived resources are
    /// not the record's and stay where they are.
    pub(super) fn scan_chunk(
        &mut self,
        chunk: ChunkNo,
        now: BTreeMap<DocId, (AppId, TypeId)>,
        before: Vec<DocId>,
    ) {
        for doc in before {
            if now.contains_key(&doc) {
                continue;
            }
            let derived = self
                .index
                .get(&doc)
                .is_some_and(|(_, ty)| lookup(ty).is_some_and(crate::schema::TypeSchema::derived));
            if !derived && self.chunks.chunk(&doc) == chunk {
                self.index.remove(&doc);
            }
        }
        for (doc, held) in now {
            self.index.insert(doc, held);
        }
    }

    /// Drop the store's writes from the `start`th on (the caller applies the rest again).
    /// Returns the collection items they added, removed or moved.
    pub(super) fn drop_local_from(&mut self, start: usize) -> ItemTouches {
        let start = start.min(self.local.len());
        let mut items = ItemTouches::new();
        for write in self.local.drain(start..) {
            for (key, id) in write.items {
                items.entry(key).or_default().insert(id);
            }
        }
        items
    }

    /// Drop the mirrors' open transactions and apply the store's committed writes again: each
    /// document as those writes left it, without whatever was written after them. Writes an
    /// origin derives stay: nothing of the record takes them back.
    pub(super) fn reapply(&mut self) {
        let chunks: Vec<ChunkNo> = self.chunks.docs().map(|(chunk, _)| chunk).collect();
        for chunk in chunks {
            self.rollback(chunk);
        }
        let local = self.local.clone();
        for write in &local {
            if let Some(mirror) = self.chunks.mirror_mut(write.chunk) {
                // Each applied once already, on this same base: one that fails now is a fault.
                if let Err(error) = apply_edits(
                    mirror,
                    &write.app,
                    std::slice::from_ref(&write.edit),
                    &write.seed,
                ) {
                    tracing::error!(app = write.app, %error, "a write did not apply again");
                }
            }
        }
    }
}

impl Inner {
    /// Derive the store's writes anew into an empty overlay: after a rollback, once an index
    /// they were written over changed, or once the record took some. What an index reflects is
    /// the index's, not derived here again: a write whose versions the record told
    /// ([`Self::settle_versions`]), and one the record accepted whose change the index its app
    /// reads from has taken in ([`Self::drop_committed_local`]), which leaves the store then.
    pub(super) fn rebuild_overlay(&mut self) {
        // What each index has yet to take in of each chunk, read once per index and chunk
        // however many writes wait on it.
        let mut behind: BTreeMap<(AppId, ChunkNo), Behind> = BTreeMap::new();
        let accepted = std::mem::take(&mut self.accepted);
        self.accepted = accepted
            .into_iter()
            .filter(|accepted| {
                let write = &accepted.write;
                let Some(held) = self.apps.get(&write.app) else {
                    return false;
                };
                !behind
                    .entry((write.app.clone(), write.chunk))
                    .or_insert_with(|| Behind::of(&held.heads, &self.chunks, write.chunk))
                    .reflects(&accepted.change)
            })
            .collect();
        let mut overlay = Overlay::default();
        if self.chunks.has_record() {
            let settled = &self.settled;
            let pending = self
                .local
                .iter()
                .filter(|write| !settled.contains(&write.seed));
            for write in pending.chain(self.accepted.iter().map(|accepted| &accepted.write)) {
                let Some(held) = self.apps.get(&write.app).filter(|held| held.answers()) else {
                    continue;
                };
                let mut sink = OverlaySink {
                    overlay: &mut overlay,
                    snapshot: Some(&held.snapshot),
                };
                let mut derive =
                    Derive::new(&self.chunks, &self.index, &self.nested, self.clock.read());
                if let Err(error) = derive.changes(&mut sink, &write.app, &write.reached()) {
                    tracing::warn!(app = write.app, %error, "a change was not indexed");
                }
                if let Some(ends) = &write.versions
                    && let Err(error) = derive.versions(&mut sink, &write.app, ends)
                {
                    tracing::warn!(app = write.app, %error, "a change's versions were not indexed");
                }
                if let Some(ends) = &write.versions {
                    derive_removed(&mut derive, &mut sink, &write.app, ends, &write.items);
                }
            }
        }
        self.overlay = Arc::new(overlay);
    }

    /// Take in the versions an action this device committed made, provisional until the
    /// record holds it: each resource it changed was last changed by its invoker at the time
    /// the invocation recorded, one it made was made then, and each of its fields it changed
    /// was changed then (`changed(r, .field)`). Reflection (`created(r)`, `updated(r)`,
    /// `changed(r, .field)`) and the lists ordered by them read these until the record's own
    /// replace them with the record's next change; the record's carry the same time, so
    /// nothing moves then. Returns the fields that changed: each resource's versions.
    pub(super) fn provisional_versions(
        &mut self,
        app: &str,
        start: usize,
        until: usize,
        note: &ChangeNote,
        made: &[Made],
    ) -> BTreeMap<DocId, Vec<FieldKey>> {
        let at = note.at.clone().unwrap_or_else(crate::host::system_time);
        let stamp = idx::reflect::Stamp {
            version: Version {
                who: note.invoker.clone(),
                at,
                action: note.action.clone(),
                run: note.group.clone(),
                from: note.from.clone(),
                count: note.count.max(1),
                // No change yet: the action was invoked on top of every change this device
                // holds, so it orders after each of them recorded in the same second.
                op: idx::reflect::PROVISIONAL,
                ..Version::default()
            },
        };
        let mut reached: BTreeMap<DocId, Vec<FieldKey>> = BTreeMap::new();
        let mut ends = super::Ends::new();
        for made in made {
            let resource = made.resource.clone();
            let end = idx::reflect::Ends::of(
                &stamp,
                &made.fields,
                made.kind,
                made.kind == tilolang_runtime::store::VersionKind::Removed,
            );
            let doc = tilolang_runtime::store::doc_of(&resource).to_owned();
            reached
                .entry(doc)
                .or_default()
                .extend(version_fields(&resource, &end));
            ends.insert(resource, end);
        }
        // Kept with the action's last write, so they go when its writes go.
        let Some(write) = self.local.get_mut(start..until).and_then(<[_]>::last_mut) else {
            return BTreeMap::new();
        };
        write.versions = Some(ends.clone());
        let items = write.items.clone();
        let touched_items: ItemTouches = {
            let mut touched = ItemTouches::new();
            for (key, id) in &items {
                touched.entry(key.clone()).or_default().insert(id.clone());
            }
            touched
        };
        if let (true, Some(held)) = (
            self.chunks.has_record(),
            self.apps.get(app).filter(|held| held.answers()),
        ) {
            let snapshot = Arc::clone(&held.snapshot);
            let mut sink = OverlaySink {
                overlay: Arc::make_mut(&mut self.overlay),
                snapshot: Some(&snapshot),
            };
            let mut derive =
                Derive::new(&self.chunks, &self.index, &self.nested, self.clock.read());
            if let Err(error) = derive.versions(&mut sink, app, &ends) {
                tracing::warn!(app, %error, "a change's versions were not indexed");
            }
            derive_removed(&mut derive, &mut sink, app, &ends, &items);
        }
        self.changed_within(&reached, &touched_items);
        reached
    }

    /// Drop the versions of the store's writes that `told` are the record's now: a change of the
    /// record's that carries the very action a write here made tells each version it made
    /// ([`Self::provisional_versions`]), and the index holds it. The write keeps what it wrote of
    /// its resources alone, so a rebuild of the overlay reads no version off the index again for
    /// a change those who read the mirror are told already. A transaction whose every version
    /// `told` holds is the record's, and stays only for the chunk record it goes to
    /// ([`Self::rebuild_overlay`]).
    pub(super) fn settle_versions(&mut self, told: &super::Ends) {
        let mut settled = BTreeSet::new();
        for write in &mut self.local {
            let Some(versions) = write.versions.as_mut() else {
                continue;
            };
            versions.retain(|resource, end| {
                !told
                    .get(resource)
                    .is_some_and(|held| held.same_invocation(end))
            });
            if versions.is_empty() {
                write.versions = None;
                settled.insert(write.seed.clone());
            }
        }
        // A write of the record's leaves `local` once its chunk record holds it, and takes its
        // seed with it.
        self.settled
            .retain(|seed| self.local.iter().any(|write| write.seed == *seed));
        self.settled.extend(settled);
    }
}

/// What an index has yet to take in of one chunk of the record.
enum Behind {
    /// It derived the chunk up to the heads the store holds: every change of it.
    Nothing,
    /// It never derived the chunk: none of its changes.
    Everything,
    /// The chunk's changes since the heads it derived it up to.
    Since(BTreeSet<ChangeHash>),
}

impl Behind {
    /// What an index stamped with `heads` has yet to take in of the chunk `chunk`. The changes
    /// since its heads are read only when it is behind the chunk's: what it has to derive yet.
    fn of(heads: &Stamps, chunks: &Chunks, chunk: ChunkNo) -> Self {
        let Some(heads) = heads.get(&chunk) else {
            return Self::Everything;
        };
        let Some(mirror) = chunks.mirror(chunk) else {
            // The chunk left the store: nothing reads it to derive a write from.
            return Self::Nothing;
        };
        let document = mirror.committed_document();
        if heads.as_slice() == document.get_heads().as_slice() {
            return Self::Nothing;
        }
        Self::Since(
            document
                .get_changes(heads)
                .iter()
                .map(automerge::Change::hash)
                .collect(),
        )
    }

    /// Whether the index holds `change`.
    fn reflects(&self, change: &ChangeHash) -> bool {
        match self {
            Self::Nothing => true,
            Self::Everything => false,
            Self::Since(since) => !since.contains(change),
        }
    }
}
