//! The workspaces a device governs, as the broker on it reads who is in them: the people's
//! pictures come from the profiles this host keeps, and nothing here copies a member list.

use std::sync::Arc;

use tilo_broker_core::services::WorkspaceAuthorityHandle;

use super::Workspaces;
use crate::apps::services::{WorkspaceMembershipFuture, WorkspaceMemberships};

/// The member list the broker on a host serves: which workspaces this host holds, and the picture
/// each person set in the profile it keeps for them. The workspace service a device runs installs
/// it, so an app's window draws an author's face without fetching a profile itself.
pub struct ProfileMemberships {
    workspaces: Workspaces,
    authority: WorkspaceAuthorityHandle,
}

impl ProfileMemberships {
    /// The membership metadata of the workspaces `authority` reads, pictured from the profiles
    /// `workspaces` keeps.
    #[must_use]
    pub fn new(workspaces: Workspaces, authority: WorkspaceAuthorityHandle) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            authority,
        })
    }
}

impl WorkspaceMemberships for ProfileMemberships {
    fn requires_workspace(&self, workspace: &str) -> bool {
        let mut held = false;
        let _ = self
            .authority
            .with_workspace(workspace, &mut |workspace| held = workspace.is_some());
        held
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }

    fn execute(
        &self,
        _workspace: String,
        _caller: tilo_user_identity::DeviceCert,
        _command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> WorkspaceMembershipFuture {
        Box::pin(async {
            Err(
                "a member's own change is written by their own device, not by the host keeping \
                 the workspace"
                    .into(),
            )
        })
    }

    fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        self.workspaces
            .person_profile_photo(person)
            .filter(|photo| !photo.is_empty())
    }
}
