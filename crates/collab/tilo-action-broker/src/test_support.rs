//! Test-only mutable policy fixture. Production hosts implement `WorkspaceAuthority`.
use crate::services::{LegacyWorkspaceProjection, WorkspaceAuthority};
use std::sync::Arc;
pub use tilo_action_policy::{Answer, GrantId, Grantee, KindSet, Level, Role, TreePath};
pub use tilo_desktop_model::{Workspace, WorkspaceId, WorkspaceSlot};

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
    pub desktop: tilo_desktop_model::DesktopTree,
    pub persist_path: Option<std::path::PathBuf>,
}
impl std::ops::Deref for WorkspaceTree {
    type Target = tilo_desktop_model::DesktopTree;
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
    pub fn authority(&self) -> crate::services::WorkspaceAuthorityHandle {
        Arc::new(self.clone())
    }
    pub fn services(&self) -> crate::services::HostServices {
        crate::services::HostServices {
            legacy_workspace: Some(Arc::new(self.clone())),
            // The workspaces' `chat.agent` records, as these fixtures keep
            // them: every agent a workspace grants anything works there,
            // kept by the workspace's owner.
            agents: Some(Arc::new(TreeAgents(self.clone()))),
            ..Default::default()
        }
    }
}
impl WorkspaceAuthority for WorkspaceTreeHandle {
    /// A test tree holds no workspace documents: none of its workspaces revokes a
    /// device.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        crate::services::workspace_in(&self.read().desktop, workspace)
            .map(|_| tilo_user_identity::RevocationList::new())
    }
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.read().desktop);
        Ok(())
    }
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.read().persist_path.as_ref().map(|p| {
            let mut n = p.clone().into_os_string();
            n.push(".invocations");
            n.into()
        })
    }
}
impl LegacyWorkspaceProjection for WorkspaceTreeHandle {
    fn record_effect(
        &self,
        workspace: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let mut tree = self.write();
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|w| w.id().to_string() == workspace)
            .ok_or("workspace missing")?;
        if !workspace
            .effect_receipts
            .iter()
            .any(|r| r.invocation == invocation)
        {
            workspace
                .effect_receipts
                .push(tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                    invocation,
                    actor,
                    context,
                    status,
                    tilo_clock::unix_now_i64(),
                ));
        }
        drop(tree);
        Ok(())
    }
}

/// A workspace's `chat.agent` record as a test holds it: which agents it
/// names, and who keeps each.
#[derive(Clone, Default)]
pub struct KeptAgents {
    kept: Vec<(String, String, String)>,
}

impl KeptAgents {
    /// The record of `workspace` naming `agent`, kept by `keeper`.
    #[must_use]
    pub fn keeping(mut self, workspace: &str, agent: &str, keeper: &str) -> Self {
        self.kept
            .push((workspace.to_owned(), agent.to_owned(), keeper.to_owned()));
        self
    }
}

impl crate::services::AgentRoster for KeptAgents {
    fn keeper(&self, workspace: &str, agent: &str) -> Option<String> {
        self.kept
            .iter()
            .find(|(held, named, _)| held == workspace && named == agent)
            .map(|(_, _, keeper)| keeper.clone())
    }
}

/// The workspaces' agents, read off the grants a fixture gave them: an agent
/// works in a workspace whose grants name it, kept by whoever owns the
/// workspace.
struct TreeAgents(crate::test_support::WorkspaceTreeHandle);

impl crate::services::AgentRoster for TreeAgents {
    fn keeper(&self, workspace: &str, agent: &str) -> Option<String> {
        let held = {
            let tree = self.0.read();
            crate::services::workspace_in(&tree.desktop, workspace)?.clone()
        };
        let node = held.node();
        let kept = held.policy.grants.iter().any(|grant| {
            grant.node == node
                && grant.answer == Answer::Allow
                && grant.subject.agent_id() == Some(agent)
        });
        if !kept {
            return None;
        }
        // A fixture workspace names its owner, or — where it has none — is
        // kept by the one person its grants speak for.
        if let Some(owner) = held.owner() {
            return Some(owner.clone());
        }
        let mut people: Vec<&str> = held
            .policy
            .grants
            .iter()
            .filter_map(|grant| grant.subject.person_id())
            .collect();
        people.sort_unstable();
        people.dedup();
        match people.as_slice() {
            [only] => Some((*only).to_owned()),
            _ => None,
        }
    }
}
