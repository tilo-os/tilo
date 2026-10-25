//! Records of the workspace's room that every app of the workspace opens
//! beside its own ([`collab_types::WORKSPACE_RECORDS`]): one per kind and
//! room, kept by one app, read and changed from any app's window.
//!
//! The workspace's emoji are one: any view draws `:name:` from them, in
//! Notes as in Chat. A window holds such a record the way it holds one of
//! the person's ([`super::person`]): opened beside its own record, in the
//! store its owner keeps in the room, edited with attribution and undo and
//! synced on the owner's route. The room's grants on the owner's resource
//! decide each change, wherever it is made.

use std::rc::Rc;

use automerge::AutoCommit;

use super::person::PersonRecord;
use super::{RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore};
use crate::permissions::UserId;

impl RecordOwners {
    /// The owner of `package_id`'s records in `room` that a window of
    /// another app opens beside its own. Like [`Self::open_person`] it
    /// never joins a native component's construction.
    ///
    /// # Errors
    /// Rejects an invalid package or room.
    pub fn open_beside(
        &mut self,
        package_id: &str,
        room: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_with(package_id, room, false)
    }
}

/// A workspace record, open: held as one of the person's is.
pub type WorkspaceRecord = PersonRecord;

/// Opens the store an app keeps in the window's room.
type OpenStore = dyn Fn(&str) -> Result<RecordStore, RecordOpenError>;

/// Where a window opens the records every app of its room opens. Cloning
/// shares them.
#[derive(Clone)]
pub struct WorkspaceRecords {
    open: Rc<OpenStore>,
    author: UserId,
}

impl WorkspaceRecords {
    /// The room's records that `owners` keep in `room`: one writer per app
    /// and room, shared with the owner's own windows in this process.
    #[must_use]
    pub fn in_room(owners: RecordOwners, room: String, author: UserId) -> Self {
        Self::new(author, move |owner| {
            owners.clone().open_beside(owner, &room)
        })
    }

    /// The room's records, in the store `open` opens for the app that keeps
    /// them: a host that syncs each store it hands out (the browser's page)
    /// notes it there.
    #[must_use]
    pub fn new(
        author: UserId,
        open: impl Fn(&str) -> Result<RecordStore, RecordOpenError> + 'static,
    ) -> Self {
        Self {
            open: Rc::new(open),
            author,
        }
    }

    #[must_use]
    pub fn author(&self) -> &UserId {
        &self.author
    }

    /// The record of `kind` that `owner` keeps in the room, created with
    /// `initialize` (its genesis, the same change on every device) where
    /// nothing holds it yet.
    ///
    /// # Errors
    /// The pair is no workspace record, or the record could not be read
    /// or created.
    pub fn open_record(
        &self,
        owner: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<WorkspaceRecord, RecordOpenError> {
        let id = collab_types::workspace_record_id(kind);
        if collab_types::workspace_record(owner, &id).is_none() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let store = (self.open)(owner)?;
        let id = RecordId::parse(&id).ok_or(RecordOpenError::IdentityMismatch)?;
        let checkout = RecordCheckout::new(id, RecordKind::new(kind));
        super::person::open_in(&store, &checkout, initialize)?;
        Ok(PersonRecord::new(store, checkout, self.author.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(store: &Rc<durable_store::MemoryStore>, author: &str) -> WorkspaceRecords {
        WorkspaceRecords::in_room(
            RecordOwners::with_store(Rc::clone(store) as Rc<dyn durable_store::DurableStore>),
            "00000000-0000-4000-8000-0000000000aa".into(),
            UserId(author.into()),
        )
    }

    fn put(doc: &mut AutoCommit, value: &str) -> Result<(), String> {
        use automerge::transaction::Transactable as _;
        doc.put(automerge::ROOT, "value", value)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_workspace_record_is_its_owners_route_in_the_room() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let record = records(&store, "alice")
            .open_record("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
            .unwrap();
        let route = record.route().expect("a room's record syncs");
        assert_eq!(route.app_id, "so.tilo.chat");
        assert_eq!(
            route.record.as_deref(),
            Some(collab_types::workspace_record_id("tilo.workspace.emoji").as_str())
        );
        assert!(
            records(&store, "alice")
                .open_record("so.tilo.notes", "tilo.workspace.emoji", &|_| Ok(()))
                .is_err(),
            "only the owner named keeps it"
        );
        assert!(
            records(&store, "alice")
                .open_record("so.tilo.chat", "chat.workspace", &|_| Ok(()))
                .is_err(),
            "the owner's other records are not the workspace's"
        );
    }

    /// Two windows of different apps in one process hold the one record:
    /// what one writes the other reads.
    #[test]
    fn every_window_of_the_room_holds_the_one_record() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let owners =
            RecordOwners::with_store(Rc::clone(&store) as Rc<dyn durable_store::DurableStore>);
        let room = "00000000-0000-4000-8000-0000000000aa".to_owned();
        let notes = WorkspaceRecords::in_room(owners.clone(), room.clone(), UserId("a".into()));
        let chat = WorkspaceRecords::in_room(owners, room, UserId("a".into()));
        let written = notes
            .open_record("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
            .unwrap();
        written.edit(true, |doc| put(doc, "from notes")).unwrap();
        let read = chat
            .open_record("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
            .unwrap();
        let value = read.read(|doc| {
            use automerge::ReadDoc as _;
            doc.get(automerge::ROOT, "value")
                .ok()
                .flatten()
                .map(|(value, _)| value.to_string())
        });
        assert_eq!(value.flatten().as_deref(), Some("\"from notes\""));
    }
}
