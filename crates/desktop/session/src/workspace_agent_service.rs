//! The workspace's record of its agents (`tilo.workspace.agent`), published as a session
//! service so a tilolang view reads and changes it as any other resource: `assistant:…agent`
//! is the roster the workspace keeps, and `assistant:…agent-permissions` says whether the person
//! at this window may pause or resume one.
//!
//! The record is Assistant's and the workspace's: the session follows it through the device's
//! own read, as the Rust settings sheet does, and answers its own actions (pause-agent,
//! resume-agent and the rest) through the device's daemon, which decides them by the workspace's
//! grants and writes them signed. Nothing here keeps a list of its own.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tilo_action_policy::WorkspacePolicy;
use tilo_actions::{ActionAddress, ActionInvoker, ApiError, ResourceSpec};
use tilo_app::tilo_workspace_agent::{AgentPermissions, KIND, OWNER, WorkspaceAgents as Record};
use tilo_desktop_surface::DesktopHandle;
use tilo_resource::Resource as _;
use tilo_service::{Service, Started};
use tilo_session_client::daemon_documents::DaemonShell;

use crate::followed_resource::FollowedResource;

/// The record of the workspace's agents, as a view names it.
pub(crate) const AGENTS: &str = "assistant:tilo.workspace.agent";
/// Whether the person may pause or resume one of the workspace's agents.
pub(crate) const PERMISSIONS: &str = "assistant:tilo.workspace.agent-permissions";

const AGENTS_LABEL: &str = "Agents";
const PERMISSIONS_LABEL: &str = "Agent permissions";

/// The actions of the record a person changes its agents' working state with.
const PAUSE: &str = "pause-agent";
const RESUME: &str = "resume-agent";

/// The record as the workspace in front holds it.
type Open = (String, WorkspacePolicy, FollowedResource<Record>);

/// The workspace's record of its agents, as this session publishes it.
pub(crate) struct WorkspaceAgentService {
    /// The device's daemon, where the record is read and its actions run.
    daemon: DaemonShell,
    /// The layout, where the workspace the person is in and its grants are read.
    handle: DesktopHandle,
    /// Who sits here: whose changes they are and whose grants decide them.
    person: String,
    /// Wakes the session's loop when the record moved.
    wake: tilo_service::Wake,
    /// Each workspace's record, followed from the first read and kept at the grants it was
    /// followed with: a workspace whose grants changed has it followed again.
    open: RefCell<BTreeMap<String, Open>>,
    /// The published resources last built, and the revision of the record they were built at.
    published: RefCell<Option<(u64, Vec<ResourceSpec>)>>,
    /// The revision last told, so a move is told once.
    seen: Cell<u64>,
}

impl WorkspaceAgentService {
    /// The record, read and changed as the person `person` wherever `handle` says they are,
    /// waking the session's loop through `wake` when it moved.
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

    /// The workspace the person is in, and its grants.
    fn workspace(&self) -> Option<(String, WorkspacePolicy)> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| (held.id().to_string(), held.policy.clone()))
        })
    }

    /// The record of the workspace in front, followed where it is not, at that workspace's
    /// grants. `None` where there is no workspace in front or it was not read yet.
    fn record(&self) -> Option<FollowedResource<Record>> {
        let (workspace, policy) = self.workspace()?;
        let mut open = self.open.borrow_mut();
        if open
            .get(&workspace)
            .is_some_and(|(_, followed, _)| *followed != policy)
        {
            open.remove(&workspace);
        }
        let (_, _, followed) = open.entry(workspace.clone()).or_insert_with(|| {
            let id = collab_types::workspace_document_id(KIND);
            let followed = FollowedResource::follow(
                &self.daemon,
                (&workspace, OWNER, &id),
                &self.person,
                Some(policy.clone()),
                std::sync::Arc::clone(&self.wake),
            );
            (workspace.clone(), policy, followed)
        });
        (followed.held() == Some(true)).then(|| followed.clone())
    }

    /// The roster as the record reads it now: each agent's name, model and whether it works.
    /// `None` where the workspace keeps no record here.
    fn reading(&self) -> Option<Value> {
        let record = self.record()?;
        Some(tilo_resource::read::reading::<Record>(&*record.get()))
    }

    /// Whether the person may pause or resume an agent: the record's own grants for the action,
    /// or the workspace's governance.
    fn permissions(&self) -> AgentPermissions {
        let manage = self.handle.read(|tree| {
            tree.active_workspace()
                .is_some_and(|workspace| {
                    use tilo_desktop::authority::WorkspaceAuthority as _;
                    workspace.may_administer(&self.person)
                })
        });
        let Some(record) = self.record() else {
            return AgentPermissions::default();
        };
        AgentPermissions {
            pause: manage || record.may(PAUSE),
            resume: manage || record.may(RESUME),
        }
    }

    /// The resources as the record publishes them now: its actions, each the record's own, and
    /// the permissions reading beside it.
    fn resource(&self) -> Vec<ResourceSpec> {
        let Some(record) = self.record() else {
            return vec![
                tilo_resource::resource::unreadable_parts::<Record>(
                    AGENTS,
                    AGENTS_LABEL,
                    "the workspace's agents are not read here now",
                ),
                tilo_service::offer::<tilo_resource::NoEffect>(
                    tilo_resource::LiveRow::typed::<PermissionsRow>(PERMISSIONS, PERMISSIONS_LABEL)
                        .into_resource(),
                ),
            ];
        };
        let revision = record.revision();
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = vec![
            tilo_resource::publish::<Record>(AGENTS, AGENTS_LABEL, &*record.get()),
            tilo_service::offer::<tilo_resource::NoEffect>(
                tilo_resource::LiveRow::typed::<PermissionsRow>(PERMISSIONS, PERMISSIONS_LABEL)
                    .into_resource(),
            ),
        ];
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    /// Carry out `action` on the record as `invoker`: the record's own action, run by the
    /// device's daemon, which decides it by the workspace's grants.
    fn act(
        &self,
        invoker: &ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        if resource != AGENTS {
            return Err(ApiError::Unknown(address));
        }
        let record = self.record().ok_or_else(|| ApiError::Unavailable {
            code: "unavailable".to_owned(),
            reason: "the workspace's agents are not read here now".to_owned(),
        })?;
        // The arguments are bound here first, so a call that cannot bind says why before it
        // reaches the daemon.
        tilo_resource::resource::build_action::<Record>(
            action,
            arguments,
            tilo_resource::origin_now_ms(),
        )
        .map_err(ApiError::InvalidArguments)?;
        record
            .invoke_as(invoker, action, arguments.clone())
            .map_err(ApiError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The row the permissions reading is published as: its fields are the reading's, and its
/// surface is built from [`AgentPermissions`] beside the record's.
#[derive(tilo_resource::Row)]
#[row(kind = "tilo.workspace.agent-permissions", label = "Agent permissions")]
struct PermissionsRow;

impl Service for WorkspaceAgentService {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.resource()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource == AGENTS {
            return Some(self.reading().unwrap_or_else(|| {
                // A workspace that keeps no record yet reads as none: a view reads its agents as
                // absent rather than failing on one.
                Value::Object(Map::from_iter([(
                    "agents".to_owned(),
                    Value::Array(Vec::new()),
                )]))
            }));
        }
        (resource == PERMISSIONS)
            .then(|| serde_json::to_value(self.permissions()).unwrap_or(Value::Null))
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

    /// What moved in the record, from this device or another: the roster is published again.
    fn poll(&mut self) -> bool {
        let Some(record) = self.record() else {
            return false;
        };
        let revision = record.revision();
        let was = self.seen.replace(revision);
        was != revision
    }
}
