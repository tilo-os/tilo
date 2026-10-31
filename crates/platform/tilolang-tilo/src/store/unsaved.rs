//! The store's **unsaved layer**: what an origin derives, which the room record never holds,
//! and the writes the store replays when it puts the mirror back (rollback, rewind).
//!
//! A type whose origin kind is derived (`type Desk (origin: session.desk)`,
//! [`crate::schema::TypeSchema::derived`]) is written into the same mirror open transaction as
//! any write — so reads see it at once — but committing it queues nothing ([`commit_derived`]).
//! It stays in the mirror's open transaction and the overlay, which is what reads answer from.
//!
//! The record moving on ([`crate::store::TiloStore::sync_from`]) drops the store's saved writes
//! (the record has them) and leaves the derived ones: [`Inner::drop_committed_local`] takes the
//! saved ones out of [`super::Inner::local`], [`Inner::rescan`] keeps the derived docs known,
//! and [`Inner::reapply_derived`] writes them onto the record's new copy again. An origin's
//! update thus never round-trips through the record: no action is committed, no peer replays
//! it, and no index is rebuilt for it.
//!
//! [`Inner::reapply`] is the plain replay of everything the store wrote, which rollback and
//! rewind use; the derived writes it leaves in place are the unsaved layer's.

use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange};

use crate::action::apply_edits;
use crate::schema::lookup;

use super::{Inner, ItemTouches, LocalWrite, TxnState, scan_of};

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
pub(super) fn commit_derived(inner: &mut Inner, state: &mut TxnState) -> bool {
    if state.saved > 0 {
        return false;
    }
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
            doc,
            fields: fields.into_iter().collect(),
            listings: own,
        });
    }
    true
}

impl Inner {
    /// Tell whoever reads what `write` reached that it changed.
    pub(super) fn touch(&mut self, write: &LocalWrite) {
        let mut reached: std::collections::BTreeMap<DocId, Vec<FieldKey>> =
            std::collections::BTreeMap::new();
        for key in write
            .touched
            .iter()
            .chain(write.items.iter().map(|(key, _)| key))
        {
            reached
                .entry(key.doc().to_owned())
                .or_default()
                .push(key.clone());
        }
        self.changed_within(&reached);
    }

    /// Derive one write into the overlay.
    pub(super) fn derive_write(&mut self, write: &LocalWrite) {
        let Some(mirror) = self.mirror.clone() else {
            return;
        };
        let Some(held) = self.apps.get(&write.app).filter(|held| held.ready()) else {
            return;
        };
        let snapshot = std::sync::Arc::clone(&held.snapshot);
        let overlay = std::sync::Arc::make_mut(&mut self.overlay);
        let mut sink = crate::index::overlay::OverlaySink {
            overlay,
            snapshot: Some(&snapshot),
        };
        let mut derive = crate::index::derive::Derive::new(&mirror, &self.index, &self.nested);
        if let Err(error) = derive.changes(&mut sink, &write.app, &write.reached()) {
            tracing::warn!(app = write.app, %error, "a change was not indexed");
        }
    }

    /// [`Inner::drop_local_from`], keeping every write an origin derives: those are never the
    /// record's, so the record moving on leaves them where they are (the caller applies them
    /// again onto its new copy, as the rollback took the whole transaction out).
    pub(super) fn drop_committed_local(&mut self) -> ItemTouches {
        let mut items = ItemTouches::new();
        let mut kept = Vec::with_capacity(self.local.len());
        for write in std::mem::take(&mut self.local) {
            if write.derived {
                kept.push(write);
                continue;
            }
            for (key, id) in write.items {
                items.entry(key).or_default().insert(id);
            }
        }
        self.local = kept;
        items
    }

    /// Write the store's derived writes onto the mirror again, as pending ops: after the
    /// record's changes reached it, which dropped everything else the store wrote. The
    /// mirror's committed state (the record's) is untouched, so nothing else rolls back.
    pub(super) fn reapply_derived(&mut self) {
        let derived: Vec<LocalWrite> = self
            .local
            .iter()
            .filter(|write| write.derived)
            .cloned()
            .collect();
        if derived.is_empty() {
            return;
        }
        if let Some(mirror) = self.mirror_mut() {
            for write in &derived {
                if let Err(error) = apply_edits(
                    mirror,
                    &write.app,
                    std::slice::from_ref(&write.edit),
                    &write.seed,
                ) {
                    tracing::error!(app = write.app, %error, "a derived write did not apply again");
                }
            }
        }
    }

    /// Re-read which resources the mirror holds: the record's, plus what an origin derives
    /// (those are in the mirror still, as its open writes, but [`super::scan_of`] reads only
    /// committed records).
    pub(super) fn rescan(&mut self) {
        let derived: Vec<(DocId, (AppId, TypeId))> = self
            .index
            .iter()
            .filter(|(_, (_, ty))| lookup(ty).is_some_and(crate::schema::TypeSchema::derived))
            .map(|(doc, held)| (doc.clone(), held.clone()))
            .collect();
        self.index = scan_of(self.mirror.as_deref());
        for (doc, held) in derived {
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

    /// Drop the mirror's open transaction and apply the store's committed writes again: the
    /// mirror as those writes left it, without whatever was written after them. Writes an
    /// origin derives stay: nothing of the record takes them back.
    pub(super) fn reapply(&mut self) {
        self.rollback();
        let local = self.local.clone();
        if let Some(mirror) = self.mirror_mut() {
            for write in &local {
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
