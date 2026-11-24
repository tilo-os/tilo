//! The window's record, named in its app's record index in the workspace.
//!
//! A workspace holds a record by naming it in the index of the app that keeps it
//! ([`ui_core::resource::index`]): the app's own record of the workspace, which the shell reads
//! to list the workspace's records and search finds them by name in. A window names the record it
//! shows there, for every app, so a document written in a window is listed wherever the
//! workspace's records are, under what the app calls it: a Writer document by its first heading.
//! A record the window made is named once something is in it: a window opened and closed again
//! leaves no blank entry behind.
//!
//! The index is a record like any other of the app: opened in the window's store, changed there,
//! and published on its own route by the window, or written by a store that writes for itself.

use std::cell::{Cell, RefCell};

use ui_core::history_mode::HistoryOwner;
use ui_core::permissions::MemberId;
use ui_core::resource::{
    DocumentCheckout, ResourceOpenError, ResourceStore, StoredResource, index,
};

/// The entry of one window's record in its app's index.
pub(crate) struct RecordIndex {
    store: ResourceStore,
    record: DocumentCheckout,
    author: MemberId,
    /// The name the app gives a record it says nothing particular of (its kind's label): an entry
    /// is named only by something the record holds.
    unnamed: String,
    /// The index, once opened: until then it is opened again at the next settle, as the workspace
    /// may still be bringing it, or the store be in use.
    index: RefCell<Option<StoredResource>>,
    /// The revision of the record's state the entry was last written for, and the name it was
    /// given then.
    noted: RefCell<Option<(u64, Option<String>)>>,
    /// The revision of a record the window made, while nothing changed it since: not named yet.
    blank_at: Cell<Option<u64>>,
    /// The index could not be opened or written for good: the window shows its record all the
    /// same.
    given_up: Cell<bool>,
}

impl RecordIndex {
    /// The entry of `record`, in the index of the app whose records `store` keeps, written as
    /// `author`; `None` for the app's own record of the workspace, which is the index itself.
    pub(crate) fn new(
        store: ResourceStore,
        record: DocumentCheckout,
        author: MemberId,
        unnamed: String,
    ) -> Option<Self> {
        (!record.id.is_package_primary()).then(|| Self {
            store,
            record,
            author,
            unnamed,
            index: RefCell::default(),
            noted: RefCell::default(),
            blank_at: Cell::new(None),
            given_up: Cell::new(false),
        })
    }

    /// The window made the record, which stands at `revision`: it is named once that moves.
    pub(crate) fn made_at(&self, revision: u64) {
        self.blank_at.set(Some(revision));
    }

    /// Name the record in the index as `name_of` says the app calls it, when the record's state
    /// moved to `revision` since it was last named. Housekeeping, on nobody's undo stack.
    pub(crate) fn note(&self, revision: u64, name_of: impl FnOnce() -> String) {
        if self.given_up.get()
            || self.blank_at.get() == Some(revision)
            || self
                .noted
                .borrow()
                .as_ref()
                .is_some_and(|(noted, _)| *noted == revision)
        {
            return;
        }
        let Some(kept) = self.opened() else {
            return;
        };
        let name = Some(name_of())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && *name != self.unnamed);
        let unchanged = self
            .noted
            .borrow()
            .as_ref()
            .is_some_and(|(_, named)| *named == name);
        if unchanged {
            *self.noted.borrow_mut() = Some((revision, name));
            return;
        }
        let record = &self.record;
        let written = kept.try_edit(false, |doc| {
            index::reference(doc, &record.id, &record.kind);
            index::set_name(doc, &record.id, name.as_deref());
            Ok(())
        });
        match written {
            Ok(Ok(())) => {
                self.blank_at.set(None);
                *self.noted.borrow_mut() = Some((revision, name));
            }
            Ok(Err(error)) => {
                tracing::warn!(record = %record.id, %error, "the record is not named in its app's index");
                self.given_up.set(true);
            }
            // The store or the index is in use now: the next settle writes the entry.
            Err(busy) if busy.is_momentary() => {}
            Err(error) => {
                tracing::warn!(record = %record.id, %error, "the record is not named in its app's index");
                self.given_up.set(true);
            }
        }
    }

    /// The index, opened now if it was not yet.
    fn opened(&self) -> Option<std::cell::Ref<'_, StoredResource>> {
        if self.index.borrow().is_none() {
            match StoredResource::record_index(
                &self.store,
                self.record.kind.clone(),
                self.author.clone(),
            ) {
                Ok(index) => *self.index.borrow_mut() = Some(index),
                Err(ResourceOpenError::Arriving | ResourceOpenError::Busy) => return None,
                Err(error) => {
                    tracing::warn!(record = %self.record.id, %error, "the app's record index could not be opened");
                    self.given_up.set(true);
                    return None;
                }
            }
        }
        std::cell::Ref::filter_map(self.index.borrow(), Option::as_ref).ok()
    }

    /// The index's owner, so the window publishes what changed in it on its route.
    pub(crate) fn owner(&self) -> Option<HistoryOwner> {
        self.index
            .borrow()
            .as_ref()
            .map(StoredResource::history_owner)
    }
}
