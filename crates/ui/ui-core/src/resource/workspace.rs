//! Records a workspace holds that every app of that workspace opens beside
//! its own ([`collab_types::WORKSPACE_DOCUMENTS`]): one per kind and
//! workspace, kept by one app, read and changed from any app's window.
//!
//! The workspace's emoji are one: any view draws `:name:` from them, in
//! Notes as in Chat. A window holds such a record as it holds any beside its
//! own ([`super::stored`]): opened beside its own record, in the
//! store its owner keeps in the workspace, edited with attribution and undo and
//! synced on the owner's route. The workspace's grants on the owner's resource
//! decide each change, wherever it is made.

use std::rc::Rc;

use automerge::AutoCommit;

use super::stored::StoredResource;
use super::{
    DocumentCheckout, DocumentId, ResourceOpenError, ResourceOwners, ResourceStore, ResourceType,
};
use tilo_ui_base::permissions::MemberId;

/// The id of the workspace's record of `kind`
/// ([`collab_types::workspace_document_id`]): the same in every workspace, which
/// scopes it.
#[must_use]
pub fn document_id(kind: &str) -> String {
    collab_types::workspace_document_id(kind)
}

/// Opens the store an app keeps in the window's workspace.
type OpenStore = dyn Fn(&str) -> Result<ResourceStore, ResourceOpenError>;

/// Where a window opens the records every app of its workspace opens. Cloning
/// shares them.
#[derive(Clone)]
pub struct WorkspaceResources {
    open: Rc<OpenStore>,
    /// Whether the record `owner` keeps, by id, is still on its way from the workspace
    /// ([`Self::awaiting`]): it opens only once the workspace answered for it.
    awaiting: Option<Rc<dyn Fn(&str, &str) -> bool>>,
    author: MemberId,
}

impl WorkspaceResources {
    /// The records of `workspace` that `owners` keep: one writer per app and
    /// workspace, shared with the owner's own windows in this process.
    #[must_use]
    pub fn in_workspace(owners: ResourceOwners, workspace: String, author: MemberId) -> Self {
        Self::new(author, move |owner| {
            owners.clone().open_beside(owner, &workspace)
        })
    }

    /// The workspace's records, in the store `open` opens for the app that keeps
    /// them: a host that syncs each store it hands out (the browser's page)
    /// notes it there.
    #[must_use]
    pub fn new(
        author: MemberId,
        open: impl Fn(&str) -> Result<ResourceStore, ResourceOpenError> + 'static,
    ) -> Self {
        Self {
            open: Rc::new(open),
            awaiting: None,
            author,
        }
    }

    /// Open a record only once the workspace answered for it: `awaiting` says
    /// whether the record `owner` keeps, by id, is still on its way. Each record
    /// waits for its own answer alone, so one the workspace answered opens while
    /// another of the same app's is still awaited.
    #[must_use]
    pub fn awaiting(self, awaiting: impl Fn(&str, &str) -> bool + 'static) -> Self {
        Self {
            awaiting: Some(Rc::new(awaiting)),
            ..self
        }
    }

    #[must_use]
    pub fn author(&self) -> &MemberId {
        &self.author
    }

    /// The record of `kind` that `owner` keeps in the workspace, created with
    /// `initialize` (its genesis, the same change on every device) where
    /// nothing holds it yet.
    ///
    /// # Errors
    /// The pair is no workspace record, or the record could not be read
    /// or created.
    pub fn open_resource(
        &self,
        owner: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        let id = collab_types::workspace_document_id(kind);
        if collab_types::workspace_document(owner, &id).is_none() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let id = DocumentId::parse(&id).ok_or(ResourceOpenError::IdentityMismatch)?;
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
    pub fn open_resource_at(
        &self,
        owner: &str,
        id: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        let id = DocumentId::parse(id).ok_or(ResourceOpenError::IdentityMismatch)?;
        self.open_at(owner, id, kind, initialize)
    }

    /// The record `owner` keeps **as its own primary record** of `kind`: the
    /// one record of a kind an app keeps in a workspace, under the owner's
    /// [`DocumentId::package_primary`] (Chat's of its chat, `chat.workspace`).
    /// [`Self::open_resource_at`] cannot open it, because the primary id is a
    /// sentinel no [`DocumentId::parse`] accepts; this is the same open with
    /// the id taken as it is.
    ///
    /// # Errors
    /// The record could not be read or created.
    pub fn open_primary(
        &self,
        owner: &str,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        self.open_at(owner, DocumentId::package_primary(), kind, initialize)
    }

    /// Whether `owner`'s store holds the record `id` of `kind`, without
    /// opening or creating it: a list asks this to show the records that
    /// exist, not to make one for everything it could name.
    ///
    /// `id` may be the [package primary](DocumentId::package_primary) sentinel
    /// that [`Self::open_primary`] opens: it is a workspace record's identity
    /// even though no route names it, so a caller checking an app's own record
    /// asks here rather than parsing it away.
    ///
    /// # Errors
    /// `id` names no record, or the record could not be read.
    pub fn holds_resource(
        &self,
        owner: &str,
        id: &str,
        kind: &str,
    ) -> Result<bool, ResourceOpenError> {
        let id = if id == DocumentId::package_primary().as_str() {
            DocumentId::package_primary()
        } else {
            DocumentId::parse(id).ok_or(ResourceOpenError::IdentityMismatch)?
        };
        let store = (self.open)(owner)?;
        let checkout = DocumentCheckout::new(id, ResourceType::new(kind));
        // A list names far more records than anyone opens: it never asks the
        // workspace for one it does not hold.
        match store.open_if_held(&checkout) {
            Ok(_) => Ok(true),
            Err(ResourceOpenError::MissingResource) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The store `owner` keeps its records of this workspace in: where a chain
    /// kept beside one of them ([`DocumentId::part`]) has its parts.
    ///
    /// # Errors
    /// Rejects a package or workspace that names no store.
    pub fn store_of(&self, owner: &str) -> Result<ResourceStore, ResourceOpenError> {
        (self.open)(owner)
    }

    /// Open `id` in the store `owner` keeps, and in this app's, so that every
    /// replica of the app publishes it.
    fn open_at(
        &self,
        owner: &str,
        id: DocumentId,
        kind: &str,
        initialize: &dyn Fn(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<StoredResource, ResourceOpenError> {
        if self
            .awaiting
            .as_ref()
            .is_some_and(|awaiting| awaiting(owner, id.as_str()))
        {
            return Err(ResourceOpenError::Arriving);
        }
        let store = (self.open)(owner)?;
        let checkout = DocumentCheckout::new(id, ResourceType::new(kind));
        super::stored::open_in(&store, &checkout, initialize)?;
        Ok(StoredResource::new(store, checkout, self.author.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window's connection to a workspace whose host has not sent the record yet.
    struct Unanswered;

    impl crate::resource::DocumentPort for Unanswered {
        fn read(
            &self,
            _: &str,
            _: &str,
            reads: Option<Vec<crate::resource::DocumentAsk>>,
        ) -> Result<Vec<(String, crate::resource::DocumentRead)>, String> {
            Ok(reads
                .unwrap_or_default()
                .into_iter()
                .map(|ask| (ask.id, crate::resource::DocumentRead::Pending))
                .collect())
        }

        fn write(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Vec<u8>,
        ) -> crate::resource::Written {
            Box::new(|_: crate::time::Duration| Some(Ok(())))
        }
    }

    fn records(owners: &ResourceOwners, author: &str) -> WorkspaceResources {
        WorkspaceResources::in_workspace(
            owners.clone(),
            "00000000-0000-4000-8000-0000000000aa".into(),
            MemberId::person(author),
        )
    }

    fn put(doc: &mut AutoCommit, value: &str) -> Result<(), String> {
        use automerge::transaction::Transactable as _;
        doc.put(automerge::ROOT, "value", value)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// A record a window holds beside its own whose history the workspace's host has not
    /// replayed is on its way: the window builds without it and asks again once it arrives.
    /// It waited for it on the window's frame, with no clock, and a host that never replayed
    /// it left the window blank and answering no action.
    #[test]
    fn a_record_beside_the_window_whose_history_never_arrives_is_on_its_way() {
        const APP: &str = "so.tilo.terminal";
        const RECORD: &str = "derived:terminal.history@tab:0000000000000000";
        let (opened, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let workspace = uuid::Uuid::new_v4();
            let manager =
                tilo_ui_base::collaboration::CollaborationManager::native_initial_unanswered_fixture_for_test(
                    APP,
                    workspace,
                    AutoCommit::new().save(),
                    &[RECORD],
                );
            let construction =
                tilo_ui_base::collaboration::native_initial::NativeConstruction::enter(manager)
                    .expect("the window's construction");
            // The app keeps the record itself: the window's own store opens it, the one that
            // reads its records through the window's broker, which has not replayed it yet.
            let store = ResourceOwners::reading(Rc::new(Unanswered))
                .open(APP, &workspace.to_string())
                .expect("the window's store");
            let records =
                WorkspaceResources::new(MemberId::person("alice"), move |_| Ok(store.clone()));
            let first = records.open_resource_at(APP, RECORD, "terminal.history", &|_| Ok(()));
            let again = records.open_resource_at(APP, RECORD, "terminal.history", &|_| Ok(()));
            drop(construction);
            let _ = opened.send([first.err(), again.err()]);
        });
        let answered = answer
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the window waited for a record its host never replayed");
        assert!(
            answered
                .iter()
                .all(|error| matches!(error, Some(ResourceOpenError::Arriving))),
            "{answered:?}"
        );
    }

    tilo_test_env::scenario! {
        fn a_workspace_holds_one_record_for_every_window_and_app;

        phase a_workspace_record_is_its_owners_route_in_the_workspace;
        {
            let owners = ResourceOwners::in_memory();
            let record = records(&owners, "alice")
                .open_resource("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
                .unwrap();
            let route = record.route().expect("a workspace's record syncs");
            assert_eq!(route.app_id, "so.tilo.chat");
            assert_eq!(
                route.document.as_deref(),
                Some(collab_types::workspace_document_id("tilo.workspace.emoji").as_str())
            );
            assert!(
                records(&owners, "alice")
                    .open_resource("so.tilo.notes", "tilo.workspace.emoji", &|_| Ok(()))
                    .is_err(),
                "only the owner named keeps it"
            );
            assert!(
                records(&owners, "alice")
                    .open_resource("so.tilo.chat", "chat.workspace", &|_| Ok(()))
                    .is_err(),
                "the owner's other records are not the workspace's"
            );
        }

        phase a_conversations_record_syncs_on_the_app_that_holds_it;
        {
            let owners = ResourceOwners::in_memory();
            let workspace = "00000000-0000-4000-8000-0000000000aa";
            let pair =
                collab_types::direct_pair_id(workspace, &"11".repeat(32), &"22".repeat(32)).unwrap();
            let id = collab_types::direct_resource(&pair);
            let record = records(&owners, "alice")
                .open_resource_at("so.tilo.chat", &id, "so.tilo.chat", &|_| Ok(()))
                .unwrap();
            let route = record.route().expect("a conversation's record syncs");
            assert_eq!(route.app_id, "so.tilo.chat");
            assert_eq!(route.document.as_deref(), Some(id.as_str()));
            assert!(
                records(&owners, "alice")
                    .open_resource_at("so.tilo.chat", "direct_nope", "so.tilo.chat", &|_| Ok(()))
                    .is_err(),
                "an id that names no record opens none"
            );
        }

        phase every_window_of_the_workspace_holds_the_one_record;
        // Two windows of different apps in one process hold the one record:
        // what one writes the other reads.
        {
            let owners = ResourceOwners::in_memory();
            let workspace = "00000000-0000-4000-8000-0000000000aa".to_owned();
            let notes =
                WorkspaceResources::in_workspace(owners.clone(), workspace.clone(), MemberId::person("a"));
            let chat = WorkspaceResources::in_workspace(owners, workspace, MemberId::person("a"));
            let written = notes
                .open_resource("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
                .unwrap();
            written.edit(true, |doc| put(doc, "from notes")).unwrap();
            let read = chat
                .open_resource("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
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

        phase a_primary_record_the_workspace_holds_is_found_by_its_sentinel_id;
        // A package's own record (Chat's chat) is kept under the primary sentinel no route names,
        // so asking whether the workspace holds it must not parse the sentinel as a record id: a
        // session that asked did, got "no such record" for the identity it had just written, and
        // read its conversations as gone for one pass — which the broker took as the turn ending
        // while a step it had started was still launching ("task stopped while waiting for its
        // application").
        {
            let owners = ResourceOwners::in_memory();
            let records = records(&owners, "alice");
            let primary = DocumentId::package_primary();
            assert!(
                !records
                    .holds_resource("so.tilo.chat", primary.as_str(), "chat.workspace")
                    .unwrap(),
                "nothing holds it before it is opened"
            );
            let store = records.store_of("so.tilo.chat").unwrap();
            store
                .create_resource(&DocumentCheckout::package_primary(ResourceType::new(
                    "chat.workspace",
                )))
                .unwrap();
            assert!(store.is_ready(&primary));
            assert!(
                records
                    .holds_resource("so.tilo.chat", primary.as_str(), "chat.workspace")
                    .unwrap(),
                "the record Chat keeps as itself is the one the sentinel names"
            );
            assert!(
                !records
                    .holds_resource("so.tilo.notes", primary.as_str(), "chat.workspace")
                    .unwrap(),
                "only its owner keeps it"
            );
        }
    }
}
