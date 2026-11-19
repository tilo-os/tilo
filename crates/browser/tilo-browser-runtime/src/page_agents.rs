//! The workspace's agents on the page, as Chat's agents resource names each: an agent is a
//! member of the workspace like a person is, and a page names it to its windows so an app's
//! people list offers it whether or not a device of its works here now.
//!
//! The resource is the one every app of the workspace opens ([`tilo_app::tilo_workspace_agent`]),
//! read here in the resources the page keeps for the workspace's windows. A page opens it
//! again each frame until it arrives, so its code loads once the page is in the workspace.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::framework::WorkspaceResource;
use tilo_app::tilo_workspace_agent::{OWNER, WorkspaceAgents};
use ui::resource::ResourceOwners;
use ui::resource::ResourceStore;

/// The workspace's agents of the workspace whose stores are `stores`, opened as `author`
/// reads them: `None` where the workspace cannot be named, so a page in no workspace names
/// none.
fn open(
    owners: &ResourceOwners,
    stores: &Rc<RefCell<Vec<ResourceStore>>>,
    workspace: &str,
    author: String,
) -> Option<WorkspaceResource<WorkspaceAgents>> {
    let resources = crate::workspace_resources::in_page(owners, stores, workspace, author, true);
    WorkspaceResource::open(
        &resources,
        OWNER,
        (tilo_app_id::apps::CHAT, "Chat"),
        None,
        None,
    )
}

/// How the agents resource is read: in the resources the page keeps for the workspace's windows,
/// whose stores are `stores`, as `author` reads them.
type AgentsOpener = fn(
    &ResourceOwners,
    &Rc<RefCell<Vec<ResourceStore>>>,
    &str,
    String,
) -> Option<WorkspaceResource<WorkspaceAgents>>;

/// The agents resource's code, as a program that fetches it on first use hands it over.
#[derive(Clone, Copy)]
pub struct MakePageAgents(AgentsOpener);

impl MakePageAgents {
    pub(crate) fn make(
        self,
        owners: &ResourceOwners,
        stores: &Rc<RefCell<Vec<ResourceStore>>>,
        workspace: &str,
        author: String,
    ) -> Option<WorkspaceResource<WorkspaceAgents>> {
        (self.0)(owners, stores, workspace, author)
    }
}

/// The agents resource this build links.
#[must_use]
pub fn builtin_page_agents() -> MakePageAgents {
    MakePageAgents(open)
}

ui::lazy_code! {
    /// The agents resource's code: linked, or with `deferred-page-agents` fetched once the page
    /// is in a workspace and asks for it.
    pub static PAGE_AGENTS: MakePageAgents = builtin_page_agents, feature = "deferred-page-agents";
}
