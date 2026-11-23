//! Test-only mutable policy fixture. Production hosts implement `WorkspaceAuthority`.
use crate::apps::services::WorkspaceAuthority;
use std::sync::Arc;
pub use tilo_action_policy::{Answer, GrantId, Grantee, KindSet, Level, Role, TreePath};
pub use tilo_desktop_model::{Workspace, WorkspaceId};

/// The grants fixtures give, in the workspace's own terms.
pub trait Grants {
    /// Let `subject` take actions of `kinds` at `relative` (a path below the
    /// workspace). Returns the grant's id.
    fn allow(&mut self, subject: Grantee, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Allow)
    }
    /// Refuse `subject` actions of `kinds` at `relative`.
    fn deny(&mut self, subject: Grantee, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Deny)
    }
    /// Give `subject` `answer` for actions of `kinds` at `relative`.
    fn answer(
        &mut self,
        subject: Grantee,
        relative: &TreePath,
        kinds: KindSet,
        answer: Answer,
    ) -> GrantId;
    /// Let `subject` take actions of `kinds` at the workspace `workspace`.
    fn allow_in_workspace(&mut self, subject: Grantee, workspace: &str, kinds: KindSet) -> GrantId;
    /// Let the agent `agent` take actions of `kinds` in the workspace.
    fn allow_agent(&mut self, agent: &str, kinds: KindSet) -> GrantId {
        self.allow(Grantee::agent(agent), &TreePath::root(), kinds)
    }
    /// Let `subject` take actions of `kinds` anywhere in the app `app`.
    fn allow_app(&mut self, subject: Grantee, app: &str, kinds: KindSet) -> GrantId {
        self.allow(subject, &TreePath::root().app(app), kinds)
    }
}

impl Grants for Workspace {
    fn answer(
        &mut self,
        subject: Grantee,
        relative: &TreePath,
        kinds: KindSet,
        answer: Answer,
    ) -> GrantId {
        let owner = self.owner().cloned().unwrap_or_default();
        self.grant(tilo_action_policy::Grant::new(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            subject,
            self.node().join(relative),
            kinds,
            answer,
            Grantee::person(owner),
        ))
    }

    fn allow_in_workspace(&mut self, subject: Grantee, workspace: &str, kinds: KindSet) -> GrantId {
        let owner = self.owner().cloned().unwrap_or_default();
        self.grant(tilo_action_policy::Grant::new(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            subject,
            TreePath::workspace(workspace),
            kinds,
            Answer::Allow,
            Grantee::person(owner),
        ))
    }
}
#[derive(Clone, Default)]
pub struct WorkspaceTree {
    pub desktop: tilo_desktop_model::Layout,
    pub persist_path: Option<std::path::PathBuf>,
}
impl std::ops::Deref for WorkspaceTree {
    type Target = tilo_desktop_model::Layout;
    fn deref(&self) -> &Self::Target {
        &self.desktop
    }
}
impl std::ops::DerefMut for WorkspaceTree {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.desktop
    }
}
#[derive(Clone)]
pub struct WorkspaceTreeHandle(Arc<parking_lot::RwLock<WorkspaceTree>>);
impl std::ops::Deref for WorkspaceTreeHandle {
    type Target = parking_lot::RwLock<WorkspaceTree>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl WorkspaceTree {
    pub fn shared(self) -> WorkspaceTreeHandle {
        WorkspaceTreeHandle(Arc::new(parking_lot::RwLock::new(self)))
    }
}
impl WorkspaceTreeHandle {
    pub fn authority(&self) -> crate::apps::services::WorkspaceAuthorityHandle {
        Arc::new(self.clone())
    }
    pub fn services(&self) -> crate::apps::services::ProcessServices {
        crate::apps::services::ProcessServices {
            journal_root: self.journal_root(),
            // The workspaces' `chat.agent` records, as these fixtures keep
            // them: every agent a workspace grants anything works there,
            // kept by the workspace's owner.
            agents: Some(Arc::new(TreeAgents(self.clone()))),
            ..Default::default()
        }
    }
    /// The fixture's journal, beside the profile it persists to, as a host
    /// keeping state keeps it.
    fn journal_root(&self) -> Option<std::path::PathBuf> {
        self.read().persist_path.as_ref().map(|p| {
            let mut n = p.clone().into_os_string();
            n.push(".invocations");
            n.into()
        })
    }
}
impl WorkspaceAuthority for WorkspaceTreeHandle {
    /// A test tree holds no workspace documents: none of its workspaces revokes a
    /// device.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        crate::apps::services::workspace_in(&self.read().desktop, workspace)
            .map(|_| tilo_user_identity::RevocationList::new())
    }
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        read(&self.read().desktop);
        Ok(())
    }
}

/// A workspace's record of its agents as a test holds it: which agents it
/// names, by workspace.
#[derive(Clone, Default)]
pub struct KeptAgents {
    listed: Vec<(String, String)>,
}

impl KeptAgents {
    /// The record of `workspace` naming `agent`.
    #[must_use]
    pub fn listing(mut self, workspace: &str, agent: &str) -> Self {
        self.listed.push((workspace.to_owned(), agent.to_owned()));
        self
    }
}

impl crate::apps::services::AgentRoster for KeptAgents {
    fn listed(&self, workspace: &str) -> Vec<crate::apps::services::ListedAgent> {
        self.listed
            .iter()
            .filter(|(held, _)| held == workspace)
            .map(|(_, agent)| crate::apps::services::ListedAgent {
                id: agent.clone(),
                name: agent.clone(),
                acts_for: String::new(),
            })
            .collect()
    }
}

/// The workspaces' agents, read off the grants a fixture gave them: an agent
/// works in a workspace whose grants name it.
struct TreeAgents(crate::apps::test_support::WorkspaceTreeHandle);

impl crate::apps::services::AgentRoster for TreeAgents {
    fn listed(&self, _: &str) -> Vec<crate::apps::services::ListedAgent> {
        Vec::new()
    }

    fn lists(&self, workspace: &str, agent: &str) -> bool {
        let tree = self.0.read();
        crate::apps::services::workspace_in(&tree.desktop, workspace).is_some_and(|held| {
            let node = held.node();
            held.policy.grants.iter().any(|grant| {
                grant.node == node
                    && grant.answer == Answer::Allow
                    && grant.grantee.agent_id() == Some(agent)
            })
        })
    }
}
