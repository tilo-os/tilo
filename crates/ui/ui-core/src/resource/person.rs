//! Records an app holds beside the one it shows.
//!
//! An app's shared data has workspace records.
//!
//! The settings sections and the few records that are a person's alone — what the keyboard learned
//! of their typing, the signature they sign with — belong to the person, not to any workspace: they
//! are kept in the person's own scope ([`PERSONAL_SCOPE`]), not in an id derived from their key.
//! What a workspace keeps for every member (its accounts, its contacts, its browsing) is the
//! workspace's ([`super::workspace`]). [`PersonResources`] is where they are
//! kept on this device for one app; [`StoredResource`] is one of them, opened, edited with
//! attribution and undo, and sent on to the person's other devices
//! ([`StoredResource::history_owner`]).
//!
//! Such a record carries one route per app and record ([`StoredResource::route`]) naming the
//! person's own cluster — the one every device of theirs derives from their identity root alone,
//! and the one their store carries between them — so the same record opened by the same app on
//! another device is the same document. On a device whose identity derives no cluster they have no
//! route and stay on that device alone.

use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};

use super::{
    DocumentCheckout, DocumentId, HistoryUnavailable, ResourceOpenError, ResourceOwners,
    ResourceStore, ResourceType,
};
use crate::history_mode::HistoryOwner;
use crate::history_mode::owner::ResourceOwnerLifetime;
use crate::permissions::MemberId;

#[cfg(test)]
#[path = "person_owner_tests.rs"]
mod owner_tests;

pub use super::PERSONAL_SCOPE;

/// The id of the person's record of `kind` for an app: one per kind, the
/// same on every device, so every device opens the same record. It plays
/// the kind's role for the app's base record rather than for a window's or
/// a workspace's, so it is the same wherever the app opens its records.
#[must_use]
pub fn document_id(kind: &ResourceType) -> DocumentId {
    DocumentId::derived(kind.as_str(), &DocumentId::for_tab(0))
}

/// Where one app keeps the person's records on this device. Cloning shares
/// the open records.
#[derive(Clone)]
pub struct PersonResources {
    store: ResourceStore,
    owners: ResourceOwners,
    /// Another window of this app holds the stored records: these are a
    /// replica that syncs with it and stores nothing.
    replica: bool,
    author: MemberId,
    /// Where the person's records of other apps are kept, when not beside
    /// these: a host that holds them itself lends them to the apps it shows
    /// ([`Self::sharing`]).
    others: Option<ResourceOwners>,
    /// The app these are the person's records of: `open`'s package, carried
    /// across [`Self::of_app`], which opens another app's.
    own_app: String,
}

impl PersonResources {
    /// `app_id`'s records of the person `author`, kept in `owners`.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn open(
        owners: &mut ResourceOwners,
        app_id: &str,
        author: MemberId,
    ) -> Result<Self, ResourceOpenError> {
        Ok(Self {
            store: owners.open_personal(app_id, &author)?,
            owners: owners.clone(),
            replica: false,
            author,
            others: None,
            own_app: app_id.to_owned(),
        })
    }

    /// Records that store nothing on this device, for a window whose app
    /// already holds the stored ones elsewhere (the same app open in another
    /// window): they converge with that window over the record's route.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn replica(app_id: &str, author: MemberId) -> Result<Self, ResourceOpenError> {
        Self::replica_in(app_id, PERSONAL_SCOPE, author)
    }

    /// [`Self::replica`], of the records kept in `scope`.
    fn replica_in(app_id: &str, scope: &str, author: MemberId) -> Result<Self, ResourceOpenError> {
        let mut owners = ResourceOwners::in_memory();
        let store = if scope == PERSONAL_SCOPE {
            owners.open_personal(app_id, &author)?
        } else {
            owners.open_person(app_id, scope)?
        };
        Ok(Self {
            store,
            owners,
            replica: true,
            author,
            others: None,
            own_app: app_id.to_owned(),
        })
    }

    /// The person's records `app_id` keeps, held in this app's store
    /// beside its own. In one host, these use the original profile-scoped
    /// owner registry; another mounted app never opens a second writer or
    /// empty fallback replica for the same record. Separate hosts/devices
    /// converge over the record's route, which a record of the person's own
    /// scope names in their own cluster. Sharing an owner adds no editor
    /// permission; the framework enforces the resource's editor list.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn of_app(&self, app_id: &str) -> Result<Self, ResourceOpenError> {
        if self.store.backend().is_none() {
            return Self::replica(app_id, self.author.clone());
        }
        let mut owners = self.others.clone().unwrap_or_else(|| self.owners.clone());
        Ok(Self {
            store: owners.open_personal(app_id, &self.author)?,
            owners,
            replica: self.replica,
            author: self.author.clone(),
            others: None,
            own_app: self.own_app.clone(),
        })
    }

    /// The id of the record of `kind` these keep: the one every device derives alike.
    fn id_of(kind: &ResourceType) -> DocumentId {
        document_id(kind)
    }

    /// These records, with the person's records of other apps
    /// ([`Self::of_app`]) opened where `others` keeps them: the host
    /// holds those itself and lends them to every app it shows in its
    /// process, so the process has one copy of each.
    #[must_use]
    pub fn sharing(mut self, others: &Self) -> Self {
        self.others = Some(others.owners.clone());
        self
    }

    /// Whether a host keeps the person's shared records and lends them to
    /// these ([`Self::sharing`]): those this app owns are then the host's
    /// too, so the process has one writer of each.
    #[must_use]
    pub fn shared_by_host(&self) -> bool {
        self.others.is_some()
    }

    /// Write out what is not stored yet, as closing the window does.
    ///
    /// The records are the store's once written: a store whose host commits
    /// in its own time (the page's, through IndexedDB) says "not yet" for
    /// every write it took, which is not a record that could not be stored.
    ///
    /// # Errors
    /// The records' writer reports a storage failure.
    pub fn flush(&self) -> Result<(), String> {
        self.store.try_flush_written()
    }

    #[must_use]
    pub fn author(&self) -> &MemberId {
        &self.author
    }

    /// Whether these are this app's own records: another app's, opened
    /// beside them ([`Self::of_app`]), are a copy this app reads.
    fn owns_resources(&self) -> bool {
        let (package, _) = self.store.app_and_home();
        package == self.own_app
    }

    /// Open the person's record of `kind`, created with `initialize` — its
    /// genesis, the same change on every device — when nothing holds it yet.
    /// When another window of the app keeps it, this window opens a replica
    /// that stores nothing and follows that window's.
    ///
    /// # Errors
    /// The record could not be read, created or replicated.
    pub fn open_resource(
        &self,
        kind: ResourceType,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        let checkout = DocumentCheckout::new(Self::id_of(&kind), kind);
        match open_in(&self.store, &checkout, initialize) {
            Err(error) => Err(error),
            Ok(()) => Ok(StoredResource::new(
                self.store.clone(),
                checkout,
                self.author.clone(),
                self.owns_resources(),
            )),
        }
    }

    /// The person's record of `kind` when it exists, without creating it:
    /// what a reader opens, leaving creation to the first write
    /// ([`Self::open_resource`]).
    ///
    /// # Errors
    /// The record could not be read or replicated.
    pub fn open_existing_resource(
        &self,
        kind: ResourceType,
    ) -> Result<Option<StoredResource>, ResourceOpenError> {
        let checkout = DocumentCheckout::new(Self::id_of(&kind), kind);
        let store = match self.store.try_open_resource(&checkout) {
            Ok(_) => self.store.clone(),
            Err(ResourceOpenError::MissingResource) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(StoredResource::new(
            store,
            checkout,
            self.author.clone(),
            self.owns_resources(),
        )))
    }

    /// Open the original writer for a host-owned canonical edit. Unlike the
    /// readable replica path, this must observe and edit the actual owner:
    /// a busy/missing writer is never replaced by an empty local replica.
    pub fn open_original_resource(
        &self,
        kind: ResourceType,
        initialize: impl Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        if self.replica {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let checkout = DocumentCheckout::new(Self::id_of(&kind), kind);
        open_in(&self.store, &checkout, &initialize)?;
        Ok(StoredResource::new(
            self.store.clone(),
            checkout,
            self.author.clone(),
            self.owns_resources(),
        ))
    }
}

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

/// A record held beside the one an app shows: the app's own of a kind, one
/// of the person's, or one of the workspace's ([`super::workspace`]), each
/// held the same way. Edits are attributed to the person, recorded as undo
/// steps of their own and synced on the record's route where it has one.
pub struct StoredResource {
    store: ResourceStore,
    checkout: DocumentCheckout,
    author: MemberId,
    /// Whether this app publishes the record's changes on its route: the
    /// records of its own package, and the workspace's shared ones. Another
    /// app's records are joined and merged from their route, never
    /// published there ([`Self::publishes`]).
    publishes: bool,
    /// Keeps the record's route installed while the record is open.
    lifetime: Rc<ResourceOwnerLifetime>,
}

impl StoredResource {
    pub(super) fn new(
        store: ResourceStore,
        checkout: DocumentCheckout,
        author: MemberId,
        publishes: bool,
    ) -> Self {
        Self {
            store,
            checkout,
            author,
            publishes,
            lifetime: ResourceOwnerLifetime::new(),
        }
    }

    /// The record index of the app whose records `store` keeps, in the store's workspace: the
    /// app's own record there ([`DocumentId::package_primary`]), which names each record of the
    /// app the workspace holds ([`super::index`]) and which the shell lists them from. Created
    /// where nothing holds it yet; the app publishes it, as it is its own.
    ///
    /// # Errors
    /// The index could not be read or created now: [`ResourceOpenError::Arriving`] while the
    /// workspace has not answered for it, [`ResourceOpenError::Busy`] while the store is in use.
    pub fn record_index(
        store: &ResourceStore,
        kind: ResourceType,
        author: MemberId,
    ) -> Result<Self, ResourceOpenError> {
        let checkout = DocumentCheckout::package_primary(kind);
        open_in(store, &checkout, &|_| Ok(()))?;
        Ok(Self::new(store.clone(), checkout, author, true))
    }

    /// Whether this app publishes the record's changes on its route.
    ///
    /// The identity service signs a change only for the route's own app, and
    /// for the workspace's shared records: never for another app's record,
    /// its keeper's or its editors'. A window of an app that only reads
    /// another app's record joins its route and merges what comes back;
    /// nothing local is sent there, and a change that was would be refused a
    /// signature — which stops the whole collaboration, not one record.
    #[must_use]
    pub fn publishes(&self) -> bool {
        self.publishes
    }

    /// Wake the window `reader` belongs to whenever another replica's change
    /// to the records kept with this one arrives
    /// ([`ResourceStore::wake_on_arrival`]).
    pub fn wake_on_arrival(&self, reader: crate::WakeHandle) {
        self.store.wake_on_arrival(reader);
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
        let mut edit = Some(edit);
        let mut out = None;
        let result = self.try_edit_with(undoable, &mut |doc| match edit.take() {
            Some(edit) => edit(doc).map(|value| out = Some(value)),
            None => Ok(()),
        })?;
        Ok(result.and_then(|()| out.ok_or_else(|| "the edit was not made".to_owned())))
    }

    /// [`Self::try_edit`] with the edit erased: one body for every edit.
    #[inline(never)]
    fn try_edit_with(
        &self,
        undoable: bool,
        edit: &mut dyn FnMut(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<Result<(), String>, HistoryUnavailable> {
        let author = self.author.clone();
        let result = self
            .store
            .with_history_or_why(&self.checkout.id, |doc, history| {
                let before = doc.get_heads();
                edit(doc)?;
                // The closure writes the record directly, naming no finer
                // action than an edit of the whole record.
                doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Native.change_message()),
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
        self.step(|doc, history, author| history.undo_as(doc, Some(author), None, None).is_some())
    }

    /// Redo the person's last undone edit of this record.
    pub fn redo(&self) -> bool {
        self.step(|doc, history, author| history.redo_as(doc, Some(author), None, None).is_some())
    }

    fn step(
        &self,
        step: impl FnOnce(&mut AutoCommit, &mut collaboration::history::UndoLog, &MemberId) -> bool,
    ) -> bool {
        let stepped = self
            .store
            .with_history(&self.checkout.id, |doc, history| {
                step(doc, history, &self.author)
            })
            .unwrap_or(false);
        if stepped && let Err(error) = self.store.schedule_committed_history(&self.checkout.id) {
            tracing::error!(%error, "a stepped record could not be saved");
        }
        stepped
    }

    /// The route this record syncs on. `None` for a record kept on this
    /// device only.
    #[must_use]
    pub fn route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.store.route(&self.checkout).ok()
    }

    /// Whether this record syncs to other devices.
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.route().is_some()
    }

    /// The record's owner for its collaboration route, which a widget
    /// reports from [`Widget::resource_owners`](crate::widget::Widget::resource_owners)
    /// so the runtime publishes its edits and merges other devices' while
    /// it is open.
    #[must_use]
    pub fn history_owner(&self) -> HistoryOwner {
        HistoryOwner::record(
            self.store.clone(),
            self.checkout.clone(),
            &self.lifetime,
            self.publishes,
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
    /// the workspace whose route carries the record. `None` for a record the
    /// person keeps in their own scope, which carries no content of its own,
    /// and for one kept on this device only.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        let (package, home) = self.store.app_and_home();
        content_of(package, &home, &self.checkout.id)
    }
}

fn content_of(package: String, home: &str, id: &DocumentId) -> Option<tilo_content::ContentStore> {
    let workspace = uuid::Uuid::parse_str(home).ok()?;
    let route = if id.is_package_primary() {
        crate::collaboration::RouteSpec::app(package, workspace)
    } else {
        crate::collaboration::RouteSpec::document(package, workspace, id.as_str())
    };
    Some(crate::collaboration::document_content::ContentLink::resource_store(&route))
}

#[cfg(test)]
mod document_id_tests {
    /// One record per kind: every device of the person derives the same id
    /// from the kind alone, so each opens the record the others have.
    #[test]
    fn a_person_record_has_one_id_per_kind() {
        let kind = crate::resource::ResourceType::new("terminal.history");
        assert_eq!(
            super::document_id(&kind),
            super::document_id(&crate::resource::ResourceType::new("terminal.history"))
        );
        assert_ne!(
            super::document_id(&kind),
            super::document_id(&crate::resource::ResourceType::new("terminal.bookmarks"))
        );
        assert_ne!(
            super::document_id(&kind),
            crate::resource::DocumentId::window_default(0),
            "the person's record of a kind is no window's"
        );
        assert_eq!(
            super::document_id(&kind).as_str(),
            collab_types::person_document_id("terminal.history"),
            "the broker admits a person record by the id apps open it under"
        );
    }
}
