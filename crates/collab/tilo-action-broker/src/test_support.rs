//! Test-only mutable policy fixture. Production hosts implement `WorkspaceAuthority`.
use crate::services::{LegacyWorkspaceProjection, WorkspaceAuthority};
use std::sync::Arc;
pub use tilo_action_policy::{Answer, GrantId, KindSet, Level, Role, Subject, TreePath};
pub use tilo_desktop_model::{RoomId, Workspace, WorkspaceId};

/// The grants fixtures give, in the room's own terms.
pub trait Grants {
    /// Let `subject` take actions of `kinds` at `relative` (a path below the
    /// room). Returns the grant's id.
    fn allow(&mut self, subject: Subject, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Allow)
    }
    /// Refuse `subject` actions of `kinds` at `relative`.
    fn deny(&mut self, subject: Subject, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Deny)
    }
    /// Give `subject` `answer` for actions of `kinds` at `relative`.
    fn answer(
        &mut self,
        subject: Subject,
        relative: &TreePath,
        kinds: KindSet,
        answer: Answer,
    ) -> GrantId;
    /// Let `subject` take actions of `kinds` in another room, `room`.
    fn allow_in_room(&mut self, subject: Subject, room: &str, kinds: KindSet) -> GrantId;
    /// Admit the agent `agent` to the room for actions of `kinds`.
    fn admit_agent(&mut self, agent: &str, kinds: KindSet) -> GrantId {
        self.allow(Subject::agent(agent), &TreePath::root(), kinds)
    }
    /// Let `subject` take actions of `kinds` anywhere in the app `app`.
    fn allow_app(&mut self, subject: Subject, app: &str, kinds: KindSet) -> GrantId {
        self.allow(subject, &TreePath::root().app(app), kinds)
    }
    /// Let `subject` take `action` on `resource` in the app `app`.
    fn allow_action(
        &mut self,
        subject: Subject,
        (app, resource, action): (&str, &str, &str),
        kinds: KindSet,
    ) -> GrantId {
        self.allow(
            subject,
            &TreePath::root()
                .app(app)
                .any(Level::Kind)
                .resource(resource)
                .action(action),
            kinds,
        )
    }
}

impl Grants for Workspace {
    fn answer(
        &mut self,
        subject: Subject,
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
            Subject::Person(owner),
        ))
    }

    fn allow_in_room(&mut self, subject: Subject, room: &str, kinds: KindSet) -> GrantId {
        let owner = self.owner().cloned().unwrap_or_default();
        self.grant(tilo_action_policy::Grant::new(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            subject,
            TreePath::room(room),
            kinds,
            Answer::Allow,
            Subject::Person(owner),
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
            ..Default::default()
        }
    }
}
impl WorkspaceAuthority for WorkspaceTreeHandle {
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
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let mut tree = self.write();
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|w| w.room.to_string() == room)
            .ok_or("room missing")?;
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
    fn record_task_update(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<tilo_desktop_model::WorkspaceTaskStatus>,
        _: String,
    ) -> Result<(), String> {
        Err("task mutation unsupported by this fixture".into())
    }
}
