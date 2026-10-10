//! Records in the person's own room: data that belongs to one person and
//! follows them across their devices, never to a collaborator.
//!
//! A shared room's records are its members'. Some of an app's state is
//! nobody's but the person's — what they watched, the words they taught
//! the keyboard, where they stopped reading — and is still data rather than
//! a preference: it grows, it has history, edits to it are undone. That
//! state is a record like any other, kept in the person's own room
//! ([`collab_types::person_room`]): a room whose id is derived from the
//! person's root key and whose members are, by that derivation, exactly the
//! devices that root certified. Nobody can be invited into it.
//!
//! An app holds the person's records beside the record it shows, whatever
//! room that one is in. [`PersonRecords`] is where they are kept on this
//! device for one app; [`PersonRecord`] is one of them, opened, edited with
//! attribution and undo, and synced on its own route
//! ([`PersonRecord::history_owner`]) to the person's other devices.
//!
//! Without a root identity there is no person room: the records are kept
//! on this device only, in a scope no route can name.

use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};

use super::{RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore};
use crate::history_mode::HistoryOwner;
use crate::history_mode::owner::LegacyHistoryLifetime;
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
        })
    }

    /// Records that store nothing on this device, for a window whose app
    /// already holds the stored ones elsewhere (the same app open in another
    /// room): they converge with that window over the record's route.
    ///
    /// # Errors
    /// Rejects an invalid package.
    pub fn replica(package_id: &str, author: UserId) -> Result<Self, RecordOpenError> {
        let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
        Ok(Self {
            store: owners.open_person(package_id, &scope_of(&author))?,
            owners,
            replica: true,
            author,
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
        let mut owners = self.owners.clone();
        Ok(Self {
            store: owners.open_person(package_id, &scope_of(&self.author))?,
            owners,
            replica: self.replica,
            author: self.author.clone(),
        })
    }

    /// Write out what is not stored yet, as closing the window does.
    ///
    /// # Errors
    /// The records' writer reports a storage failure.
    pub fn flush(&self) -> Result<(), String> {
        self.store.try_flush()
    }

    /// The person these records belong to.
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
        initialize: impl Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<PersonRecord, RecordOpenError> {
        let checkout = RecordCheckout::new(record_id(&kind), kind);
        match open_in(&self.store, &checkout, &initialize) {
            Err(RecordOpenError::Store(app_state::StoreError::WriterBusy { .. }))
                if !self.replica =>
            {
                let (package, _) = self.store.package_and_home();
                let replica = Self::replica(&package, self.author.clone())?;
                open_in(&replica.store, &checkout, &initialize)?;
                Ok(PersonRecord::new(
                    replica.store,
                    checkout,
                    self.author.clone(),
                ))
            }
            Err(error) => Err(error),
            Ok(()) => Ok(PersonRecord::new(
                self.store.clone(),
                checkout,
                self.author.clone(),
            )),
        }
    }

    /// Open the original writer for a host-owned canonical edit. Unlike the
    /// readable replica path, this must observe and edit the actual owner:
    /// a busy/missing writer is never replaced by an empty local replica.
    /// # Errors
    /// Refuses a replica owner and propagates the original writer's errors.
    pub fn open_original_record(
        &self,
        kind: RecordKind,
        initialize: impl Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<PersonRecord, RecordOpenError> {
        if self.replica {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let checkout = RecordCheckout::new(record_id(&kind), kind);
        open_in(&self.store, &checkout, &initialize)?;
        Ok(PersonRecord::new(
            self.store.clone(),
            checkout,
            self.author.clone(),
        ))
    }
}

fn open_in(
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

/// One of the person's records, open. Edits are attributed to the person,
/// recorded as undo steps of their own and synced on the record's route.
pub struct PersonRecord {
    store: RecordStore,
    checkout: RecordCheckout,
    author: UserId,
    /// Keeps the record's route installed while the record is open.
    lifetime: Rc<LegacyHistoryLifetime>,
}

impl PersonRecord {
    fn new(store: RecordStore, checkout: RecordCheckout, author: UserId) -> Self {
        Self {
            store,
            checkout,
            author,
            lifetime: LegacyHistoryLifetime::new(),
        }
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
    ///
    /// # Errors
    /// The record is busy, or `edit` failed (nothing is recorded then).
    pub fn edit<T>(
        &self,
        undoable: bool,
        edit: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<T, String> {
        let author = self.author.clone();
        let result = self
            .store
            .with_history(&self.checkout.id, |doc, history| {
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
            })
            .ok_or_else(|| "the record is not open".to_owned())?;
        self.store.schedule_committed_history(&self.checkout.id);
        result
    }

    /// Undo the person's last edit of this record. Whether there was one.
    pub fn undo(&self) -> bool {
        self.step(|doc, history, author| {
            history
                .undo_as(doc, Some(author), None, None, None)
                .is_some()
        })
    }

    /// Redo the person's last undone edit of this record.
    pub fn redo(&self) -> bool {
        self.step(|doc, history, author| {
            history
                .redo_as(doc, Some(author), None, None, None)
                .is_some()
        })
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
        if stepped {
            self.store.schedule_committed_history(&self.checkout.id);
        }
        stepped
    }

    /// The route this record syncs on: the person's room, when they have
    /// one. `None` for a record kept on this device only.
    #[must_use]
    pub fn route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.store.route(&self.checkout).ok()
    }

    /// Whether this record syncs to the person's other devices.
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.route().is_some()
    }

    /// The record's owner for its collaboration route, which a widget
    /// reports from [`Widget::record_owners`](crate::widget::Widget::record_owners)
    /// so the runtime publishes its edits and merges the person's other
    /// devices' while it is open.
    #[must_use]
    pub fn history_owner(&self) -> HistoryOwner {
        HistoryOwner::legacy(self.store.clone(), self.checkout.clone(), &self.lifetime)
    }

    /// Merge a whole document of this record another device holds, as its
    /// route would. For hosts and tests that carry the record themselves.
    ///
    /// # Errors
    /// The bytes are not this record's document.
    pub fn merge_peer_document(&self, document: Vec<u8>) -> Result<bool, String> {
        let changed = self.store.merge_peer_document(&self.checkout, document)?;
        if changed {
            self.store.schedule_committed_history(&self.checkout.id);
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
    /// the person's own room, kept by the host and carried to the person's
    /// other devices on the record's route — never in the room of the
    /// window showing it. `None` without a person room, where the record
    /// stays on this device and keeps no content of its own.
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
