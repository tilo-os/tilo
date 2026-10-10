//! Adapter over the genuine enrolled fsync owner; no cache mutation substitute.
use std::{sync::Arc, time::SystemTime};
use tilo_action_broker::services::{
    WorkspaceAuthority, WorkspaceMemberships, WorkspaceTaskAgentInvocation,
};
use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceId};
use tilo_peer_state::{
    owner_commit::{CommitFuture, OwnerCommitRequest, OwnerCommitService},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskSnapshot},
};

#[derive(Clone)]
pub(super) struct Native {
    pub owner: tilo_joined_room::NativeWorkspaceOwner,
    pub room: RoomId,
}
impl WorkspaceAuthority for Native {
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
        let tree = self.snapshot()?;
        read(&tree);
        Ok(())
    }

    fn snapshot(&self) -> Result<DesktopTree, String> {
        let mut workspace = self.owner.with_current(self.room, |view| {
            let document = view
                .peer()
                .admitted_room()
                .ok_or("admitted native document absent")?;
            let (title, owner, capabilities) =
                tilo_peer_state::room::extract_governed_fields(document);
            let mut workspace = Workspace::new(
                WorkspaceId(1),
                title.unwrap_or_else(|| "Qualification".into()),
            );
            workspace.room = self.room;
            workspace.owner = owner;
            workspace.capabilities = capabilities;
            Ok(workspace)
        })?;
        workspace.tasks = self.owner.workspace_task_snapshot(self.room)?.tasks;
        Ok(DesktopTree {
            workspaces: vec![workspace],
            ..Default::default()
        })
    }
}
impl OwnerCommitService for Native {
    fn requires_room(&self, room: &str) -> bool {
        room == self.room.to_string()
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let owner = self.owner.clone();
        Box::pin(async move { owner.commit_workspace_task_command(request, SystemTime::now()) })
    }
}
impl WorkspaceMemberships for Native {
    fn requires_room(&self, room: &str) -> bool {
        OwnerCommitService::requires_room(self, room)
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn task_snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String> {
        if !OwnerCommitService::requires_room(self, room) {
            return Err("native room mismatch".into());
        }
        self.owner.workspace_task_snapshot(self.room)
    }
    fn task_agent_authority(
        &self,
        invocation: &WorkspaceTaskAgentInvocation,
    ) -> Result<WorkspaceTaskAuthority, String> {
        if !OwnerCommitService::requires_room(self, invocation.room()) {
            return Err("native Agent room mismatch".into());
        }
        let attestor = self
            .owner
            .with_current(self.room, |view| Ok(view.device().clone()))?;
        Ok(WorkspaceTaskAuthority::Agent {
            attestor,
            agent_id: invocation.agent_id().into(),
        })
    }
    fn execute(&self, request: OwnerCommitRequest) -> CommitFuture {
        OwnerCommitService::commit(self, request)
    }
}
impl tilo_action_broker::peer_actions::CertificateAuthority for Native {
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if room.to_string() != self.room.to_string() {
            return Err("native certificate room mismatch".into());
        }
        self.owner.with_current(self.room, |view| {
            if view.device() != certificate {
                return Err("actual native host certificate differs".into());
            }
            Ok(())
        })
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(room, certificate)
    }
}

pub(super) fn installed(
    installation_root: &std::path::Path,
) -> Result<
    (
        Arc<Native>,
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
    ),
    String,
> {
    let owner = tilo_joined_room::open_file(&installation_root.join("native.json"))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
    let certificate = tilo_joined_room::testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
        &key,
        "Enrolled qualification governor",
    );
    let _governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)?;
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).ok_or("fixture room absent")?;
    Ok((Arc::new(Native { owner, room }), key, certificate))
}
