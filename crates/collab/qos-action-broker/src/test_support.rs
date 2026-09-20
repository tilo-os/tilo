//! Test-only mutable policy fixture. Production hosts implement WorkspaceAuthority.
use crate::services::{LegacyWorkspaceProjection, WorkspaceAuthority};
pub use collab_types::{CapOverride, Subject, Verb};
pub use qos_desktop::{RoomId, Workspace, WorkspaceId};
use std::sync::Arc;
#[derive(Clone, Default)]
pub struct WorkspaceTree {
    pub desktop: qos_desktop::WorkspaceTree,
    pub persist_path: Option<std::path::PathBuf>,
}
impl std::ops::Deref for WorkspaceTree {
    type Target = qos_desktop::WorkspaceTree;
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
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        Ok(self.read().desktop.clone())
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
                .push(crate::policy::redacted_effect_receipt(
                    invocation, actor, context, status,
                ));
        }
        Ok(())
    }
    fn record_task_update(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<qos_desktop::WorkspaceTaskStatus>,
        _: String,
    ) -> Result<(), String> {
        Err("task mutation unsupported by this fixture".into())
    }
}
