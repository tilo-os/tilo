//! Records a workspace holds that every app of that workspace opens beside
//! its own ([`collab_types::WORKSPACE_RECORDS`]): one per kind and
//! workspace, kept by one app, read and changed from any app's window.
//!
//! The workspace's emoji are one: any view draws `:name:` from them, in
//! Notes as in Chat. A window holds such a record the way it holds one of
//! the person's ([`super::person`]): opened beside its own record, in the
//! store its owner keeps in the workspace, edited with attribution and undo and
//! synced on the owner's route. The workspace's grants on the owner's resource
//! decide each change, wherever it is made.

use std::rc::Rc;

use automerge::AutoCommit;

use super::person::StoredRecord;
use super::{RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore};
use crate::permissions::UserId;

impl RecordOwners {
    /// The owner of `package_id`'s records in `workspace` that a window of
    /// another app opens beside its own. Like [`Self::open_person`] it
    /// never joins a native component's construction.
    ///
    /// # Errors
    /// Rejects an invalid package or workspace.
    pub fn open_beside(
        &mut self,
        package_id: &str,
        workspace: &str,
    ) -> Result<RecordStore, RecordOpenError> {
        self.open_with(package_id, workspace, false)
    }
}

/// The id of the workspace's record of `kind`
/// ([`collab_types::workspace_record_id`]): the same in every workspace, which
/// scopes it.
#[must_use]
pub fn record_id(kind: &str) -> String {
    collab_types::workspace_record_id(kind)
}

/// The id of the record the two people of `pair` share
/// ([`collab_types::direct_record`]).
#[must_use]
pub fn direct_record_id(pair: &str) -> String {
    collab_types::direct_record(pair)
}

/// Opens the store an app keeps in the window's workspace.
type OpenStore = dyn Fn(&str) -> Result<RecordStore, RecordOpenError>;

/// Where a window opens the records every app of its workspace opens. Cloning
/// shares them.
#[derive(Clone)]
pub struct WorkspaceRecords {
    open: Rc<OpenStore>,
    author: UserId,
}

impl WorkspaceRecords {
    /// The records of `workspace` that `owners` keep: one writer per app and
    /// workspace, shared with the owner's own windows in this process.
    #[must_use]
    pub fn in_workspace(owners: RecordOwners, workspace: String, author: UserId) -> Self {
        Self::new(author, move |owner| {
            owners.clone().open_beside(owner, &workspace)
        })
    }

    /// The workspace's records, in the store `open` opens for the app that keeps
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

    /// The record of `kind` that `owner` keeps in the workspace, created with
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
    ) -> Result<StoredRecord, RecordOpenError> {
        let id = collab_types::workspace_record_id(kind);
        if collab_types::workspace_record(owner, &id).is_none() {
            return Err(RecordOpenError::IdentityMismatch);
        }
        let id = RecordId::parse(&id).ok_or(RecordOpenError::IdentityMismatch)?;
        self.open_at(owner, id, kind, initialize)
    }

    /// The record the two people of `pair`
    /// ([`collab_types::direct_pair_id`]) share in the workspace, kept by
    /// `owner`, created with `initialize` where nothing holds it yet. Only
    /// their devices read or change it, whichever app holds it open: the
    /// workspace's grants decide nothing here.
    ///
    /// # Errors
    /// `pair` names no pair, or the record could not be read or created.
    pub fn open_direct(
        &self,
        owner: &str,
        pair: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        let id = RecordId::of_direct(pair).ok_or(RecordOpenError::IdentityMismatch)?;
        self.open_at(owner, id, kind, initialize)
    }

    /// Open `id` in the store `owner` keeps, and in this app's, so that every
    /// replica of the app publishes it.
    fn open_at(
        &self,
        owner: &str,
        id: RecordId,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        let store = (self.open)(owner)?;
        let checkout = RecordCheckout::new(id, RecordKind::new(kind));
        super::person::open_in(&store, &checkout, initialize)?;
        // Every app of the workspace publishes the record, whichever app wrote
        // it first: the workspace's grants on the owner's resource decide each
        // change.
        Ok(StoredRecord::new(
            store,
            checkout,
            self.author.clone(),
            true,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(store: &Rc<durable_store::MemoryStore>, author: &str) -> WorkspaceRecords {
        WorkspaceRecords::in_workspace(
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
    fn a_workspace_record_is_its_owners_route_in_the_workspace() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let record = records(&store, "alice")
            .open_record("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
            .unwrap();
        let route = record.route().expect("a workspace's record syncs");
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

    #[test]
    fn a_pairs_record_syncs_on_the_app_that_holds_it() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let workspace = "00000000-0000-4000-8000-0000000000aa";
        let pair =
            collab_types::direct_pair_id(workspace, &"11".repeat(32), &"22".repeat(32)).unwrap();
        let record = records(&store, "alice")
            .open_direct("so.tilo.chat", &pair, "so.tilo.chat", &|_| Ok(()))
            .unwrap();
        let route = record.route().expect("a pair's record syncs");
        assert_eq!(route.app_id, "so.tilo.chat");
        assert_eq!(
            route.record.as_deref(),
            Some(collab_types::direct_record(&pair).as_str())
        );
        assert!(
            records(&store, "alice")
                .open_direct("so.tilo.chat", "direct_nope", "so.tilo.chat", &|_| Ok(()))
                .is_err(),
            "a key that names no pair names no record"
        );
    }

    /// Two windows of different apps in one process hold the one record:
    /// what one writes the other reads.    #[test]
    fn every_window_of_the_workspace_holds_the_one_record() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let owners =
            RecordOwners::with_store(Rc::clone(&store) as Rc<dyn durable_store::DurableStore>);
        let workspace = "00000000-0000-4000-8000-0000000000aa".to_owned();
        let notes =
            WorkspaceRecords::in_workspace(owners.clone(), workspace.clone(), UserId("a".into()));
        let chat = WorkspaceRecords::in_workspace(owners, workspace, UserId("a".into()));
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
