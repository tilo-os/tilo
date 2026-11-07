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

    /// The record `id` of `kind` that `owner` keeps in the workspace, created
    /// with `initialize` where nothing holds it yet, whatever names it: a
    /// conversation's record is one, named by the two people it is between.
    /// The workspace's grants on the owner's node decide every change to it
    /// like any other record of the owner's.
    ///
    /// # Errors
    /// `id` names no record, or the record could not be read or created.
    pub fn open_record_at(
        &self,
        owner: &str,
        id: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        let id = RecordId::parse(id).ok_or(RecordOpenError::IdentityMismatch)?;
        self.open_at(owner, id, kind, initialize)
    }

    /// The record `owner` keeps **as its own primary record** of `kind`: the
    /// one record of a kind an app keeps in a workspace, under the owner's
    /// [`RecordId::package_primary`] (Chat's of its chat, `chat.workspace`).
    /// [`Self::open_record_at`] cannot open it, because the primary id is a
    /// sentinel no [`RecordId::parse`] accepts; this is the same open with
    /// the id taken as it is.
    ///
    /// # Errors
    /// The record could not be read or created.
    pub fn open_primary(
        &self,
        owner: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredRecord, RecordOpenError> {
        self.open_at(owner, RecordId::package_primary(), kind, initialize)
    }

    /// Whether `owner`'s store holds the record `id` of `kind`, without
    /// opening or creating it: a list asks this to show the records that
    /// exist, not to make one for everything it could name.
    ///
    /// # Errors
    /// `id` names no record, or the record could not be read.
    pub fn holds_record(&self, owner: &str, id: &str, kind: &str) -> Result<bool, RecordOpenError> {
        let id = RecordId::parse(id).ok_or(RecordOpenError::IdentityMismatch)?;
        let store = (self.open)(owner)?;
        let checkout = RecordCheckout::new(id, RecordKind::new(kind));
        match store.try_open_record(&checkout) {
            Ok(_) => Ok(true),
            Err(RecordOpenError::MissingRecord) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The store `owner` keeps its records of this workspace in: where a chain
    /// kept beside one of them ([`RecordId::part`]) has its parts.
    ///
    /// # Errors
    /// Rejects a package or workspace that names no store.
    pub fn store_of(&self, owner: &str) -> Result<RecordStore, RecordOpenError> {
        (self.open)(owner)
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
    fn a_conversations_record_syncs_on_the_app_that_holds_it() {
        let store = Rc::new(durable_store::MemoryStore::new());
        let workspace = "00000000-0000-4000-8000-0000000000aa";
        let pair =
            collab_types::direct_pair_id(workspace, &"11".repeat(32), &"22".repeat(32)).unwrap();
        let id = collab_types::direct_record(&pair);
        let record = records(&store, "alice")
            .open_record_at("so.tilo.chat", &id, "so.tilo.chat", &|_| Ok(()))
            .unwrap();
        let route = record.route().expect("a conversation's record syncs");
        assert_eq!(route.app_id, "so.tilo.chat");
        assert_eq!(route.record.as_deref(), Some(id.as_str()));
        assert!(
            records(&store, "alice")
                .open_record_at("so.tilo.chat", "direct_nope", "so.tilo.chat", &|_| Ok(()))
                .is_err(),
            "an id that names no record opens none"
        );
    }

    /// Two windows of different apps in one process hold the one record:
    /// what one writes the other reads.
    #[test]
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
