//! The workspace's resources as the page reads them: what a window opens beside its own (the
//! workspace's emoji, the person's drafts), the workspace's agents, its files as search finds
//! them, and who its governance says is in it.
//!
//! None of it is needed to draw the shell, so the browser desktop fetches this code as a module
//! of its own as the page starts ([`WORKSPACE_RESOURCES`], `deferred-workspace-resources`),
//! beside the first frame. A window that opens before it is in shows its app arriving, and is
//! built once it is in.

use std::cell::RefCell;
use std::rc::Rc;

use durable_store::DurableStore;
use tilo_app::framework::WorkspaceResource;
use tilo_app::tilo_workspace_agent::{OWNER, WorkspaceAgents};
use ui::application::ProcessContext;
use ui::permissions::MemberId;
use ui::resource::workspace::WorkspaceResources;
use ui::resource::{ResourceOwners, ResourceStore};

/// The stores a page keeps of one workspace, and whose they are opened as.
#[derive(Clone)]
pub(crate) struct PageStores {
    pub owners: ResourceOwners,
    /// The stores the page flushes and replicates.
    pub stores: Rc<RefCell<Vec<ResourceStore>>>,
    pub workspace: String,
    pub author: String,
    /// Whether the devices keeping the workspace send what is missing here.
    pub replicated: bool,
}

impl PageStores {
    /// Each resource in its owner's store of the workspace: the one a window of the owner
    /// opened, or opened now and kept among the page's stores, so it reaches every window of
    /// the workspace and every peer. In a replicated workspace a store's resources come from
    /// the devices keeping the workspace ([`ResourceStore::ask_workspace_for_missing`]).
    ///
    /// Only code that loads after the start module calls this.
    pub(crate) fn resources(&self) -> WorkspaceResources {
        let PageStores {
            owners,
            stores,
            workspace,
            author,
            replicated,
        } = self.clone();
        WorkspaceResources::new(MemberId::person(author), move |owner| {
            let home = (owner.to_owned(), workspace.clone());
            if let Some(store) = stores
                .borrow()
                .iter()
                .find(|store| store.app_and_home() == home)
            {
                return Ok(store.clone());
            }
            let store = owners.clone().open_beside(owner, &workspace)?;
            if replicated {
                store.ask_workspace_for_missing();
            }
            stores.borrow_mut().push(store.clone());
            Ok(store)
        })
    }
}

/// The workspace's agents as a page names them.
pub(crate) trait Agents {
    /// Each active agent's id and name, once the workspace delivered them.
    fn named(&self) -> Option<Vec<(String, String)>>;
}

/// How a window is given what it opens beside its own resource.
type Beside = fn(&mut ProcessContext, &PageStores, &Rc<dyn DurableStore>) -> Result<(), String>;

/// How a workspace held on the page is named, and given its people, from its governance.
type Governance =
    fn(u32, tilo_desktop_model::WorkspaceId, &mut tilo_desktop_model::Workspace) -> bool;

/// The code that reads the workspace's resources, as a program that fetches it on first use
/// hands it over.
#[derive(Clone, Copy)]
pub struct WorkspaceResourcesCode {
    /// Give a window the workspace's resources, and its app's of the person kept in the store.
    pub(crate) beside: Beside,
    pub(crate) flush_person: fn(&Rc<dyn DurableStore>),
    /// The workspace's agents: `None` where the workspace cannot be named.
    pub(crate) agents: fn(&PageStores) -> Option<Rc<dyn Agents>>,
    pub(crate) refresh_files: fn(&crate::page_files::WorkspaceFiles),
    /// Name a workspace and its people as the peer's governance of it says; whether it changed.
    pub(crate) project_governance: Governance,
}

fn beside(
    context: &mut ProcessContext,
    page: &PageStores,
    store: &Rc<dyn DurableStore>,
) -> Result<(), String> {
    context.provide_workspace_resources(page.resources());
    crate::person_resources::provide(context, store)
}

impl Agents for WorkspaceResource<WorkspaceAgents> {
    fn named(&self) -> Option<Vec<(String, String)>> {
        let state = self.try_get()?;
        Some(
            state
                .active()
                .filter_map(|agent| {
                    let name = agent.name.trim();
                    (!name.is_empty()).then(|| (agent.id.0.clone(), name.to_owned()))
                })
                .collect(),
        )
    }
}

/// Chat's agents resource, which every app of the workspace opens.
fn agents(page: &PageStores) -> Option<Rc<dyn Agents>> {
    let resources = PageStores {
        replicated: true,
        ..page.clone()
    }
    .resources();
    let agents = WorkspaceResource::<WorkspaceAgents>::open(
        &resources,
        OWNER,
        (tilo_app_id::apps::CHAT, "Chat"),
        None,
        None,
    )?;
    Some(Rc::new(agents))
}

fn project_governance(
    peer: u32,
    workspace: tilo_desktop_model::WorkspaceId,
    held: &mut tilo_desktop_model::Workspace,
) -> bool {
    let name = crate::replication::governance_name(peer, workspace);
    let policy = crate::replication::governance_policy(peer, workspace);
    let governed = crate::replication::governance_people(peer, workspace);
    let named = name.as_deref().is_some_and(|name| held.named_as(name));
    let people = policy
        .as_ref()
        .is_some_and(|policy| tilo_session_client::membership::project(held, policy));
    let given = governed
        .as_deref()
        .is_some_and(|governed| tilo_session_client::membership::project_names(held, governed));
    named | people | given
}

/// The code this build links.
#[must_use]
pub fn builtin_workspace_resources() -> WorkspaceResourcesCode {
    WorkspaceResourcesCode {
        beside,
        flush_person: crate::person_resources::flush::<dyn DurableStore>,
        agents,
        refresh_files: crate::page_files::WorkspaceFiles::refresh,
        project_governance,
    }
}

ui::lazy_code! {
    /// The code that reads the workspace's resources: linked, or with
    /// `deferred-workspace-resources` fetched as the page starts.
    pub static WORKSPACE_RESOURCES: WorkspaceResourcesCode = builtin_workspace_resources, feature = "deferred-workspace-resources";
}
