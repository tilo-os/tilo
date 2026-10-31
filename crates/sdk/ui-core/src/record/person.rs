//! Records an app holds beside the one it shows.
//!
//! Some of an app's state is still data rather than a preference — what was
//! watched, the words taught to the keyboard, where reading stopped — and is
//! one record of its kind: it grows, it has history, edits to it are undone.
//! That record is kept with the app's other records in the workspace it is
//! open in ([`PersonRecords::in_workspace`]), and syncs with the workspace like
//! any of them. It is each member's own there ([`RecordId::of_member`]): one
//! per member, read and changed by that person's devices alone.
//!
//! The person's shared records — their accounts, which Settings keeps for
//! all of their apps, their contacts — and the settings sections are kept in
//! the person's room ([`collab_types::person_room()`]), a room whose id is
//! derived from the person's root key. [`PersonRecords`] is where they are
//! kept on this device for one app; [`StoredRecord`] is one of them, opened,
//! edited with attribution and undo, and synced on its own route
//! ([`StoredRecord::history_owner`]) to the person's other devices.
//!
//! Without a root identity there is no person room: the records are kept
//! on this device only, in a scope no route can name.

use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};

use super::{
    HistoryUnavailable, RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners,
    RecordStore,
};
use crate::history_mode::HistoryOwner;
use crate::history_mode::owner::RecordOwnerLifetime;
use crate::permissions::UserId;

#[cfg(test)]
#[path = "person_owner_tests.rs"]
mod owner_tests;

/// The scope of a person's records on a device that knows no root
/// identity: kept here, never routed (it is not a room id).
const DEVICE_ONLY: &str = "personal";

/// The room the person `author` names, as a record scope: their person room
/// when `author` is a root identity, else a device-only scope.
#[must_use]
pub fn scope_of(author: &UserId) -> String {
    collab_types::person_room_of(&author.0)
        .map_or_else(|| DEVICE_ONLY.to_owned(), |room| room.to_string())
}

/// The id of the person's record of `kind` for an app: one per kind, the
/// same on every device, so every device opens the same record.
#[must_use]
pub fn record_id(kind: &RecordKind) -> RecordId {
    RecordId::derived(kind.as_str(), &RecordId::room_default())
}

/// Where one app keeps the person's records on this device. Cloning shares
/// the open records.
#[derive(Clone)]
pub struct PersonRecords {
    store: RecordStore,
    owners: RecordOwners,
    /// Another window of this app holds the stored records: these are a
    /// replica that syncs with it and stores nothing.
    replica: bool,
    author: UserId,
    /// Where the person's records of other apps are kept, when not beside
    /// these: a host that holds them itself lends them to the apps it shows
    /// ([`Self::sharing`]).
    others: Option<RecordOwners>,
    /// These are the app's own records in a workspace, each the author's
    /// own there ([`RecordId::of_member`]).
    member: bool,
}

impl PersonRecords {
    /// `package_id`'s records of the person `author`, kept in `owners`.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn open(
        owners: &mut RecordOwners,
        package_id: &str,
        author: UserId,
    ) -> Result<Self, RecordOpenError> {
        Ok(Self {
            store: owners.open_person(package_id, &scope_of(&author))?,
            owners: owners.clone(),
            replica: false,
            author,
            others: None,
            member: false,
        })
    }

    /// Records that store nothing on this device, for a window whose app
    /// already holds the stored ones elsewhere (the same app open in another
    /// room): they converge with that window over the record's route.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn replica(package_id: &str, author: UserId) -> Result<Self, RecordOpenError> {
        let scope = scope_of(&author);
        Self::replica_in(package_id, &scope, author)
    }

    /// [`Self::replica`], of the records kept in `scope`.
    fn replica_in(package_id: &str, scope: &str, author: UserId) -> Result<Self, RecordOpenError> {
        let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
        Ok(Self {
            store: owners.open_person(package_id, scope)?,
            owners,
            replica: true,
            author,
            others: None,
            member: false,
        })
    }

    /// The person's records `package_id` keeps, held in this app's store
    /// beside its own. In one host, these use the original profile-scoped
    /// owner registry; another mounted app never opens a second writer or
    /// empty fallback replica for the same record. Separate hosts/devices
    /// still converge over the record's route. Sharing an owner adds no
    /// editor permission; the framework enforces the resource's editor list.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn of_package(&self, package_id: &str) -> Result<Self, RecordOpenError> {
        if self.store.backend().is_none() {
            return Self::replica(package_id, self.author.clone());
        }
        let mut owners = self.others.clone().unwrap_or_else(|| self.owners.clone());
        Ok(Self {
            store: owners.open_person(package_id, &scope_of(&self.author))?,
            owners,
            replica: self.replica,
            author: self.author.clone(),
            others: None,
            member: false,
        })
    }

    /// The app's own records of a kind in the workspace `workspace`, kept
    /// where these are: one per kind and member ([`RecordId::of_member`]),
    /// the same on each of the author's devices, synced on the ordinary
    /// route of a record of the app in that workspace and read or changed
    /// by the author's devices alone. They are kept apart from the store of
    /// the record the app shows, so an edit of one while that one is being
    /// changed waits for nothing. An author without a root identity is a
    /// member of no workspace: theirs stay on this device.
    ///
    /// # Errors
    /// Rejects an invalid workspace.
    pub fn in_workspace(&self, workspace: &str) -> Result<Self, RecordOpenError> {
        if RecordId::of_member(&self.author.0, "kind").is_none() {
            return Ok(Self {
                others: None,
                ..self.clone()
            });
        }
        let (package, _) = self.store.package_and_home();
        let records = if self.store.backend().is_none() {
            Self::replica_in(&package, workspace, self.author.clone())?
        } else {
            let mut owners = self.owners.clone();
            Self {
                store: owners.open_person(&package, workspace)?,
                owners,
                replica: self.replica,
                author: self.author.clone(),
                others: None,
                member: false,
            }
        };
        Ok(Self {
            member: true,
            ..records
        })
    }

    /// The id of the record of `kind` these keep: the author's own in the
    /// workspace for an app's own records ([`Self::in_workspace`] keeps
    /// them only for a root identity), else the one every device derives
    /// alike.
    fn id_of(&self, kind: &RecordKind) -> RecordId {
        self.member
            .then(|| RecordId::of_member(&self.author.0, kind.as_str()))
            .flatten()
            .unwrap_or_else(|| record_id(kind))
    }

    /// These records, with the person's records of other apps
    /// ([`Self::of_package`]) opened where `others` keeps them: the host
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
    pub fn author(&self) -> &UserId {
        &self.author
    }

    /// The person's room, when they have one: the records sync to their
    /// other devices only then.
    #[must_use]
    pub fn room(&self) -> Option<uuid::Uuid> {
        let (_, home) = self.store.package_and_home();
        uuid::Uuid::parse_str(&home).ok()
    }

    /// Open the person's record of `kind`, created with `initialize` — its
    /// genesis, the same change on every device — when nothing holds it yet.
    /// When another window of the app keeps it, this window opens a replica
    /// that stores nothing and follows that window's.
    ///
    /// # Errors
    /// The record could not be read, created or replicated.
    pub fn open_record(
        &self,
        kind: RecordKind,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        let checkout = RecordCheckout::new(self.id_of(&kind), kind);
        match open_in(&self.store, &checkout, initialize) {
            Err(RecordOpenError::Store(app_state::StoreError::WriterBusy { .. }))
                if !self.replica =>
            {
                let (package, home) = self.store.package_and_home();
                let replica = Self {
                    member: self.member,
                    ..Self::replica_in(&package, &home, self.author.clone())?
                };
                open_in(&replica.store, &checkout, initialize)?;
                Ok(StoredRecord::new(
                    replica.store,
                    checkout,
                    self.author.clone(),
                ))
            }
            Err(error) => Err(error),
            Ok(()) => Ok(StoredRecord::new(
                self.store.clone(),
                checkout,
                self.author.clone(),
            )),
        }
    }

    /// The person's record of `kind` when it exists, without creating it:
    /// what a reader opens, leaving creation to the first write
    /// ([`Self::open_record`]).
    ///
    /// # Errors
    /// The record could not be read or replicated.
    pub fn open_existing_record(
        &self,
        kind: RecordKind,
    ) -> Result<Option<StoredRecord>, RecordOpenError> {
        let checkout = RecordCheckout::new(self.id_of(&kind), kind);
        let store = match self.store.try_open_record(&checkout) {
            Ok(_) => self.store.clone(),
            Err(RecordOpenError::MissingRecord) => return Ok(None),
            Err(RecordOpenError::Store(app_state::StoreError::WriterBusy { .. }))
                if !self.replica =>
            {
                let (package, home) = self.store.package_and_home();
                let replica = Self {
                    member: self.member,
                    ..Self::replica_in(&package, &home, self.author.clone())?
                };
                match replica.store.try_open_record(&checkout) {
                    Ok(_) => replica.store,
                    Err(RecordOpenError::MissingRecord) => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        Ok(Some(StoredRecord::new(
            store,
            checkout,
            self.author.clone(),
        )))
    }

    /// Open the original writer for a host-owned canonical edit. Unlike the
    /// readable replica path, this must observe and edit the actual owner:
    /// a busy/missing writer is never replaced by an empty local replica.
    pub fn open_original_record(
        &self,
        kind: RecordKind,
        initialize: impl Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        if self.replica {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let checkout = RecordCheckout::new(self.id_of(&kind), kind);
        open_in(&self.store, &checkout, &initialize)?;
        Ok(StoredRecord::new(
            self.store.clone(),
            checkout,
            self.author.clone(),
        ))
    }
}

pub(super) fn open_in(
    store: &RecordStore,
    checkout: &RecordCheckout,
    initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
) -> Result<(), RecordOpenError> {
    match store.try_open_record(checkout) {
        Ok(_) => Ok(()),
        Err(RecordOpenError::MissingRecord) => store
            .reserve_record(checkout)?
            .initialize(|doc| initialize(doc)),
        Err(error) => Err(error),
    }
}

/// A record held beside the one an app shows: the app's own of a kind, one
/// of the person's, or one of the workspace's ([`super::workspace`]), each
/// held the same way. Edits are attributed to the person, recorded as undo
/// steps of their own and synced on the record's route.
pub struct StoredRecord {
    store: RecordStore,
    checkout: RecordCheckout,
    author: UserId,
    /// Keeps the record's route installed while the record is open.
    lifetime: Rc<RecordOwnerLifetime>,
}

impl StoredRecord {
    pub(super) fn new(store: RecordStore, checkout: RecordCheckout, author: UserId) -> Self {
        Self {
            store,
            checkout,
            author,
            lifetime: RecordOwnerLifetime::new(),
        }
    }

    /// Wake the window `reader` belongs to whenever another replica's change
    /// to the records kept with this one arrives
    /// ([`RecordStore::wake_on_arrival`]).
    pub fn wake_on_arrival(&self, reader: crate::WakeHandle) {
        self.store.wake_on_arrival(reader);
    }

    /// The record's identity.
    #[must_use]
    pub fn checkout(&self) -> &RecordCheckout {
        &self.checkout
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
        let author = self.author.clone();
        let result = self
            .store
            .with_history_or_why(&self.checkout.id, |doc, history| {
                let before = doc.get_heads();
                let value = edit(doc)?;
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
                Ok(value)
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
        step: impl FnOnce(&mut AutoCommit, &mut crate::state::UndoLog, &UserId) -> bool,
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
    /// reports from [`Widget::record_owners`](crate::widget::Widget::record_owners)
    /// so the runtime publishes its edits and merges other devices' while
    /// it is open.
    #[must_use]
    pub fn history_owner(&self) -> HistoryOwner {
        HistoryOwner::record(self.store.clone(), self.checkout.clone(), &self.lifetime)
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
    /// the record's room, kept by the host and carried to other devices on
    /// the record's route. `None` for a record kept on this device only,
    /// which keeps no content of its own.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        let (package, home) = self.store.package_and_home();
        let room = uuid::Uuid::parse_str(&home).ok()?;
        Some(
            crate::collaboration::record_content::ContentLane::record_store(
                &crate::collaboration::RouteSpec::record(package, room, self.checkout.id.as_str()),
            ),
        )
    }
}

#[cfg(test)]
mod record_id_tests {
    /// Hosts name a person record by the id `collab_types` derives; it is
    /// the one the record is opened under.
    #[test]
    fn a_person_record_has_the_id_hosts_derive() {
        let kind = crate::record::RecordKind::new("terminal.history");
        assert_eq!(
            super::record_id(&kind).as_str(),
            collab_types::person_record_id("terminal.history")
        );
    }
}
