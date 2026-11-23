//! The workspace's contact directory as a session service: one record every member reads and
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
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_action_policy::{MemberAccess, WorkspacePolicy};
use tilo_actions::{ActionAddress, ActionInvoker, ApiError, ResourceSpec};
use tilo_app::framework::WorkspaceResource;
use tilo_contact_directory::ContactDirectory;
use tilo_desktop_surface::DesktopHandle;
use tilo_resource::{Resource as _, SharedResource as _};
use tilo_service::{Service, Started};
use ui::permissions::{LivePermissions, PermissionContext};
use ui::resource::workspace::WorkspaceResources;

use crate::engine_records::EngineRecords;

/// The resource the directory is published as: the kind itself, under the app that keeps it, as
/// the session publishes every workspace record's app.
pub(crate) const RESOURCE: &str = "contacts:tilo.contacts.identities";
/// The label a reader sees for it.
const LABEL: &str = "Contacts";
/// How this session names itself in the record's history.
const SESSION_APP: (&str, &str) = ("tilo-session", "Contacts");

/// The workspace's contact directory, as this session publishes it.
pub(crate) struct Contacts {
    /// The workspaces' records, as the workspace holds them.
    records: EngineRecords,
    /// The layout, where the workspace the person is in and its grants are read.
    handle: DesktopHandle,
    /// Who sits here: whose steps the changes are, and whose grants are read.
    person: String,
    /// Wakes the session's loop when a change to the record lands.
    wake: ui::WakeScope,
    /// Each workspace's directory, opened here and kept at the grants it was opened with.
    open: RefCell<BTreeMap<String, Opened>>,
    /// The resource published last, and the revision of the reading it was built at.
    published: RefCell<Option<(u64, ResourceSpec)>>,
    /// The revision of the reading last published, so a move is told once.
    seen: Cell<u64>,
}

struct Opened {
    /// The grants the record was opened with: a workspace whose grants changed has it opened
    /// again, so its people's reads follow.
    policy: WorkspacePolicy,
    records: WorkspaceResources,
    /// `None` until it opens: the workspace's directory, opened once and kept.
    record: Option<WorkspaceResource<ContactDirectory>>,
}

impl Contacts {
    /// The workspace's directory, read and changed as the person `person` wherever `handle` says
    /// they are, waking the session's loop through `wake` when a change lands.
    pub(crate) fn new(
        records: EngineRecords,
        handle: DesktopHandle,
        person: &str,
        wake: tilo_service::Wake,
    ) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the workspace's contacts are not woken");
        }
        Self {
            records,
            handle,
            person: person.to_owned(),
            wake: scope,
            open: RefCell::new(BTreeMap::new()),
            published: RefCell::new(None),
            seen: Cell::new(0),
        }
    }

    /// The workspace the person is in, and its grants.
    fn workspace(&self) -> Option<(String, WorkspacePolicy)> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| (held.id().to_string(), held.policy.clone()))
        })
    }

    /// The directory of the workspace the person is in, opened where it is not, at that
    /// workspace's grants. `None` where the workspace keeps none or cannot be read now.
    fn record(&self) -> Option<WorkspaceResource<ContactDirectory>> {
        let (workspace, policy) = self.workspace()?;
        let mut open = self.open.borrow_mut();
        if open
            .get(&workspace)
            .is_some_and(|opened| opened.policy != policy)
        {
            open.remove(&workspace);
        }
        let opened = open.entry(workspace.clone()).or_insert_with(|| Opened {
            policy: policy.clone(),
            records: self.records.in_workspace(
                &workspace,
                ui::permissions::MemberId::person(self.person.clone()),
            ),
            record: None,
        });
        if opened.record.is_none() {
            let id = collab_types::workspace_document_id(ContactDirectory::KIND);
            match opened.records.holds_resource(
                ContactDirectory::OWNER,
                &id,
                ContactDirectory::KIND,
            ) {
                Ok(false) => return None,
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "the workspace's contacts are not read");
                    return None;
                }
            }
            let permissions =
                PermissionContext::from_access(MemberAccess::person(policy, self.person.clone()))
                    .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
            opened.record = WorkspaceResource::<ContactDirectory>::open(
                &opened.records,
                ContactDirectory::OWNER,
                SESSION_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
        }
        opened.record.clone()
    }

    /// What the directory reads as now: its people, labels and hints, as a view of another
    /// language reads the record. `None` where the workspace keeps none.
    fn reading(&self) -> Option<Value> {
        let record = self.record()?;
        Some(tilo_resource::read::reading::<ContactDirectory>(
            &record.get(),
        ))
    }

    /// The reading as the record's resource publishes it: its actions, each the directory's own.
    fn resource(&self) -> ResourceSpec {
        let Some(record) = self.record() else {
            return tilo_resource::resource::unreadable_parts::<ContactDirectory>(
                RESOURCE,
                LABEL,
                "the workspace's contacts are not read here now",
            );
        };
        let revision = record.revision();
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = tilo_resource::publish::<ContactDirectory>(RESOURCE, LABEL, &record.get());
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
        let record = self.record().ok_or_else(|| ApiError::Unavailable {
            code: "unavailable".to_owned(),
            reason: "the workspace's contacts are not read here now".to_owned(),
        })?;
        let action = tilo_resource::resource::build_action::<ContactDirectory>(
            action,
            arguments,
            tilo_resource::origin_now_ms(),
        )
        .map_err(ApiError::InvalidArguments)?;
        record
            .act_as(invoker, &action)
            .map_err(|error| ApiError::Failed(error.to_string()))?;
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
        let Some(record) = self.record() else {
            return false;
        };
        let revision = record.revision();
        let was = self.seen.replace(revision);
        was != revision
    }
}
