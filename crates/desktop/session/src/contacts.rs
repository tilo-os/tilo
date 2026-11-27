//! The workspace's contact directory as a session service: one resource every member reads and
//! changes, published as `contacts:tilo.contacts.identities`.
//!
//! Contacts keeps the directory (`tilo_contact_directory`) and every app of the workspace opens
//! it beside its own records ([`collab_types::WORKSPACE_DOCUMENTS`]). A tilolang view, an agent
//! and a script reach it here instead of through the app's window: the reading is the record's
//! own fields, and its actions are the ones Contacts publishes (`project-contact`,
//! `confirm-endpoint`, `confirm-account`, and the rest), so the account-association panel of a
//! tilolang Contacts calls them as they are.
//!
//! The record is the workspace's, opened at the grants of the workspace the person is in: its
//! people are everyone's, and the workspace's grants on Contacts' node decide each change. No
//! member keeps a directory of their own.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionInvoker, ApiError, ResourceSpec};
use tilo_contact_directory::ContactDirectory;
use tilo_resource::{Resource as _, SharedResource as _};
use tilo_service::{Service, Started};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_session_core::desktop::state::DesktopHandle;

use crate::followed_resource::FollowedResource;

/// The resource the directory is published as: the type itself, under the app that keeps it, as
/// the session publishes every workspace resource's app.
pub(crate) const RESOURCE: &str = "contacts:tilo.contacts.identities";
/// The label a reader sees for it.
const LABEL: &str = "Contacts";

/// The workspace's contact directory, as this session publishes it.
pub(crate) struct Contacts {
    /// The device's daemon, where the directory is read and its actions run.
    daemon: DaemonShell,
    /// The layout, where the workspace the person is in and its grants are read.
    handle: DesktopHandle,
    /// Who sits here: whose changes they are, and whose grants are read.
    person: String,
    /// Wakes the session's loop when a change to the directory lands.
    wake: tilo_service::Wake,
    /// Each workspace's directory, followed from the first read and kept at the grants it was
    /// followed with: a workspace whose grants changed has it followed again.
    open: RefCell<BTreeMap<String, FollowedResource<ContactDirectory>>>,
    /// The resource published last, and the reading it was built at.
    published: RefCell<Option<(u64, ResourceSpec)>>,
    /// The reading last published, so a move is told once.
    seen: Cell<u64>,
}

impl Contacts {
    /// The workspace's directory, read and changed as the person `person` wherever `handle` says
    /// they are, waking the session's loop through `wake` when a change lands.
    pub(crate) fn new(
        daemon: DaemonShell,
        handle: DesktopHandle,
        person: &str,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            daemon,
            handle,
            person: person.to_owned(),
            wake,
            open: RefCell::new(BTreeMap::new()),
            published: RefCell::new(None),
            seen: Cell::new(0),
        }
    }

    /// The workspace the person is in.
    fn workspace(&self) -> Option<String> {
        self.handle
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
    }

    /// The directory of the workspace the person is in, followed where it is not. `None` where the workspace keeps none or it was not read yet.
    fn directory(&self) -> Option<FollowedResource<ContactDirectory>> {
        let workspace = self.workspace()?;
        let mut open = self.open.borrow_mut();
        let followed = open.entry(workspace.clone()).or_insert_with(|| {
            let id = collab_types::workspace_document_id(ContactDirectory::KIND);
            FollowedResource::follow(
                &self.daemon,
                (&workspace, ContactDirectory::OWNER, &id),
                &self.person,
                std::sync::Arc::clone(&self.wake),
            )
        });
        (followed.held() == Some(true)).then(|| followed.clone())
    }

    /// What the directory reads as now: its people, labels and hints, as a view of another
    /// language reads the record. `None` where the workspace keeps none.
    fn reading(&self) -> Option<Value> {
        let directory = self.directory()?;
        Some(tilo_resource::read::reading::<ContactDirectory>(
            &*directory.get(),
        ))
    }

    /// The reading as the record's resource publishes it: its actions, each the directory's own.
    fn resource(&self) -> ResourceSpec {
        let Some(directory) = self.directory() else {
            return tilo_resource::resource::unreadable_parts::<ContactDirectory>(
                RESOURCE,
                LABEL,
                "the workspace's contacts are not read here now",
            );
        };
        let revision = directory.revision();
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published =
            tilo_resource::publish::<ContactDirectory>(RESOURCE, LABEL, &*directory.get());
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    /// Carry out `action` on the directory as `invoker`.
    fn act(
        &self,
        invoker: &ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        if resource != RESOURCE {
            return Err(ApiError::Unknown(address));
        }
        let directory = self.directory().ok_or_else(|| ApiError::Unavailable {
            code: "unavailable".to_owned(),
            reason: "the workspace's contacts are not read here now".to_owned(),
        })?;
        // The arguments are bound here first, so a call that cannot bind says why before it
        // reaches the daemon.
        tilo_resource::resource::build_action::<ContactDirectory>(
            action,
            arguments,
            tilo_resource::origin_now_ms(),
        )
        .map_err(ApiError::InvalidArguments)?;
        directory
            .invoke_as(invoker, action, arguments.clone())
            .map_err(ApiError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

impl Service for Contacts {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![self.resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            self.reading().unwrap_or_else(|| {
                // A workspace that keeps no directory yet reads as none: a view
                // reads its fields as absent rather than failing on one.
                Value::Object(Map::from_iter([(
                    "people".to_owned(),
                    Value::Array(Vec::new()),
                )]))
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let invoker = ActionInvoker::member(tilo_actions::MemberId::person(self.person.clone()));
        self.act(&invoker, resource, action, arguments)
    }

    fn invoke_as(
        &mut self,
        invoker: &ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        self.act(invoker, resource, action, arguments)
    }

    /// What moved in the directory, from this device or another: the reading is published again.
    fn poll(&mut self) -> bool {
        let Some(directory) = self.directory() else {
            return false;
        };
        let revision = directory.revision();
        let was = self.seen.replace(revision);
        was != revision
    }
}
