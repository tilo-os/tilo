//! A record a window holds beside the one it shows: the app's own record of a kind, or one
//! every app of the workspace opens ([`super::workspace`]), held the same way: opened in the
//! store its app keeps in the workspace, edited with attribution and undo, and synced on its
//! route.

use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};

use super::{DocumentCheckout, DocumentId, HistoryUnavailable, ResourceOpenError, ResourceStore};
use crate::history_mode::HistoryOwner;
use crate::history_mode::owner::ResourceOwnerLifetime;
use tilo_ui_base::permissions::{Acting, MemberId};

/// Open `checkout` in `store`, created with `initialize` (its genesis, the same change on every
/// device) where nothing holds it yet.
pub(super) fn open_in(
    store: &ResourceStore,
    checkout: &DocumentCheckout,
    initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
) -> Result<(), ResourceOpenError> {
    match store.try_open_beside(checkout) {
        Ok(_) => Ok(()),
        Err(ResourceOpenError::MissingResource) => store
            .reserve_resource(checkout)?
            .initialize(|doc| initialize(doc)),
        Err(error) => Err(error),
    }
}

/// A record held beside the one an app shows: the app's own of a kind, or one
/// of the workspace's ([`super::workspace`]), each held the same way. Edits are attributed to the person, recorded as undo
/// steps of their own and synced on the record's route where it has one.
pub struct StoredResource {
    store: ResourceStore,
    checkout: DocumentCheckout,
    author: MemberId,
    /// Keeps the record's route installed while the record is open.
    lifetime: Rc<ResourceOwnerLifetime>,
}

impl StoredResource {
    pub(super) fn new(store: ResourceStore, checkout: DocumentCheckout, author: MemberId) -> Self {
        Self {
            store,
            checkout,
            author,
            lifetime: ResourceOwnerLifetime::new(),
        }
    }

    /// Wake the window `reader` belongs to whenever another replica's change
    /// to the records kept with this one arrives
    /// ([`ResourceStore::wake_on_arrival`]).
    pub fn wake_on_arrival(&self, reader: crate::WakeHandle) {
        self.store.wake_on_arrival(reader);
    }

    /// Who its edits are attributed to unless an edit names someone else
    /// ([`Self::try_edit_by`]).
    #[must_use]
    pub fn author(&self) -> &MemberId {
        &self.author
    }

    /// The record's identity.
    #[must_use]
    pub fn checkout(&self) -> &DocumentCheckout {
        &self.checkout
    }

    /// Whether the record's current changes have reached its durable store.
    pub fn save_status(&self) -> Result<super::ResourceSaveStatus, ResourceOpenError> {
        self.store.save_status(&self.checkout.id)
    }

    /// Read the record's document; `None` while its owner is busy.
    pub fn read<T>(&self, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        self.store.read_checked(&self.checkout.id, read).ok()
    }

    /// The record's heads: they change with every edit here or merged from
    /// another device.
    #[must_use]
    pub fn heads(&self) -> Vec<ChangeHash> {
        self.read(|doc| crate::history_mode::sorted_heads(doc.base_heads()))
            .unwrap_or_default()
    }

    /// Edit the record: `edit` changes the document, written and synced
    /// like any edit. An `undoable` change is one undo step of the
    /// person's; another (content from outside, housekeeping) is nobody's.
    pub fn edit<T>(
        &self,
        undoable: bool,
        edit: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<T, String> {
        match self.try_edit(undoable, edit) {
            Ok(result) => result,
            Err(unavailable) => Err(unavailable.to_string()),
        }
    }

    /// [`Self::edit`], telling a record that cannot be changed right now
    /// ([`HistoryUnavailable`]) from an edit that refused: `edit` did not
    /// run then, and may run once whatever holds the record lets go.
    ///
    /// # Errors
    /// Why the record cannot be changed now.
    pub fn try_edit<T>(
        &self,
        undoable: bool,
        edit: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<Result<T, String>, HistoryUnavailable> {
        self.try_edit_by(&self.author.clone(), undoable, edit)
    }

    /// [`Self::try_edit`] as `author`'s step: whoever invoked the change (the person, or an agent
    /// acting for them), whose undo ([`Self::undo_as`]) takes it back.
    ///
    /// # Errors
    /// Why the record cannot be changed now.
    pub fn try_edit_by<T>(
        &self,
        author: &MemberId,
        undoable: bool,
        edit: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<Result<T, String>, HistoryUnavailable> {
        let mut edit = Some(edit);
        let mut out = None;
        let result = self.try_edit_with(author, undoable, &mut |doc| match edit.take() {
            Some(edit) => edit(doc).map(|value| out = Some(value)),
            None => Ok(()),
        })?;
        Ok(result.and_then(|()| out.ok_or_else(|| "the edit was not made".to_owned())))
    }

    /// [`Self::try_edit`] with the edit erased: one body for every edit.
    #[inline(never)]
    fn try_edit_with(
        &self,
        author: &MemberId,
        undoable: bool,
        edit: &mut dyn FnMut(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<Result<(), String>, HistoryUnavailable> {
        let author = author.clone();
        let acting = Acting::member(author.clone());
        let result = self
            .store
            .with_history_or_why(&self.checkout.id, |doc, history| {
                let before = doc.get_heads();
                edit(doc)?;
                // The closure writes the record directly, naming no finer action than an edit of
                // the whole record: it names the author, whose edit it is.
                let message = tilo_actions::ActionContext::whole_edit_by(&acting, &*doc);
                doc.commit_with(
                    automerge::transaction::CommitOptions::default().with_message(message),
                );
                if undoable {
                    history.record_committed_action_authored(doc, before, None, None, Some(author));
                } else {
                    history.rebase(doc);
                }
                Ok(())
            })?;
        if let Err(error) = self.store.schedule_committed_history(&self.checkout.id) {
            tracing::error!(%error, "an edited record could not be saved");
        }
        Ok(result)
    }

    /// Undo the person's last edit of this record. Whether there was one.
    pub fn undo(&self) -> bool {
        self.undo_as(&self.author.clone())
    }

    /// Redo the person's last undone edit of this record.
    pub fn redo(&self) -> bool {
        self.redo_as(&self.author.clone())
    }

    /// Undo `author`'s last edit of this record, read from the record's history by who made
    /// each step. Whether there was one.
    pub fn undo_as(&self, author: &MemberId) -> bool {
        self.step(|doc, history| history.undo_as(doc, Some(author), None, None).is_some())
    }

    /// Redo `author`'s last undone edit of this record.
    pub fn redo_as(&self, author: &MemberId) -> bool {
        self.step(|doc, history| history.redo_as(doc, Some(author), None, None).is_some())
    }

    fn step(
        &self,
        step: impl FnOnce(&mut AutoCommit, &mut collab_types::history::UndoLog) -> bool,
    ) -> bool {
        let stepped = self
            .store
            .with_history(&self.checkout.id, step)
            .unwrap_or(false);
        if stepped && let Err(error) = self.store.schedule_committed_history(&self.checkout.id) {
            tracing::error!(%error, "a stepped record could not be saved");
        }
        stepped
    }

    /// The route this record syncs on. `None` for a record kept on this
    /// device only.
    #[must_use]
    pub fn route(&self) -> Option<tilo_ui_base::collaboration::RouteSpec> {
        self.store.route(&self.checkout).ok()
    }

    /// The record's owner for its collaboration route, which a widget
    /// reports from [`BoundWidget::resource_owners`](crate::bound_dispatch::BoundWidget::resource_owners)
    /// so the runtime publishes its edits and merges other devices' while
    /// it is open.
    #[must_use]
    pub fn history_owner(&self) -> HistoryOwner {
        HistoryOwner::record(
            self.store.clone(),
            self.checkout.clone(),
            &self.lifetime,
            // Every app of the workspace publishes the record: the workspace's grants on its
            // owner's resource decide each change.
            true,
        )
    }

    /// Merge a whole document of this record another device holds, as its
    /// route would. For hosts and tests that carry the record themselves.
    ///
    /// # Errors
    /// The bytes are not this record's document.
    pub fn merge_peer_document(&self, document: Vec<u8>) -> Result<bool, String> {
        let changed = self.store.merge_peer_document(&self.checkout, document)?;
        if changed && let Err(error) = self.store.schedule_committed_history(&self.checkout.id) {
            tracing::error!(%error, "a merged record could not be saved");
        }
        Ok(changed)
    }

    /// The whole record, as this device would send it to another.
    #[must_use]
    pub fn document(&self) -> Option<Vec<u8>> {
        self.store
            .replication_snapshot(&self.checkout.id)
            .ok()
            .map(|(bytes, _)| bytes)
    }

    /// Write out what is not stored yet.
    pub fn flush(&self) {
        let _ = self.store.try_flush();
    }

    /// Where the content this record references is stored and read: under
    /// the workspace whose route carries the record. `None` for one kept on
    /// this device only.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        let (package, home) = self.store.app_and_home();
        content_of(package, &home, &self.checkout.id)
    }
}

/// Where the content the record `id` that `package` keeps in the workspace `home` references is
/// stored and read: what a reader holding no copy of the record reads it from. `None` for a home
/// that names no workspace.
#[must_use]
pub fn content_of(
    package: String,
    home: &str,
    id: &DocumentId,
) -> Option<tilo_content::ContentStore> {
    let workspace = uuid::Uuid::parse_str(home).ok()?;
    let route = if id.is_package_primary() {
        tilo_ui_base::collaboration::RouteSpec::app(package, workspace)
    } else {
        tilo_ui_base::collaboration::RouteSpec::document(package, workspace, id.as_str())
    };
    Some(tilo_ui_base::collaboration::document_content::ContentLink::resource_store(&route))
}
