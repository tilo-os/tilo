//! Every workspace this profile takes part in, as the brokers ask about them:
//! the profile's own workspace, whose record holds what this profile keeps of
//! it (the person's grants to apps, effect records), and the workspaces it
//! joined, the profile's own workspace included once its host placed it.
use super::{joined::Joined, workspace::ProfileWorkspace};
use std::sync::Arc;
use tilo_action_broker::services::WorkspaceAuthority;
use tilo_desktop_model::{DesktopTree, RoomId, Workspace};
use tilo_identity_host::broker::WorkspaceMembership;

pub(super) struct Workspaces {
    pub(super) profile: Arc<ProfileWorkspace>,
    pub(super) joined: Arc<Joined>,
}

impl Workspaces {
    pub(super) fn new(profile: &Arc<ProfileWorkspace>, joined: &Arc<Joined>) -> Self {
        Self {
            profile: Arc::clone(profile),
            joined: Arc::clone(joined),
        }
    }
}

impl WorkspaceAuthority for Workspaces {
    /// The profile's own workspaces, then each joined workspace as its
    /// governance describes it. Joined workspaces are never written to the
    /// profile's workspace file, so a profile that joined any workspace reads
    /// a merged copy; one that joined none borrows its own tree.
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
        if self.joined.is_empty() {
            return self.profile.with_tree(read);
        }
        let mut tree = self.profile.snapshot()?;
        let joined: Vec<_> = self
            .joined
            .workspaces()
            .into_iter()
            .filter(|joined| {
                !tree
                    .workspaces
                    .iter()
                    .any(|own| own.room() == joined.room())
            })
            .collect();
        tree.workspaces.extend(joined);
        read(&tree);
        Ok(())
    }

    /// The profile's own workspace first, as in the merged tree; otherwise only
    /// the requested joined workspace is built.
    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        let mut read = Some(read);
        WorkspaceAuthority::with_workspace(self.profile.as_ref(), workspace, &mut |held| {
            if held.is_some()
                && let Some(read) = read.take()
            {
                read(held);
            }
        })?;
        let Some(read) = read else {
            return Ok(());
        };
        let joined = RoomId::parse(workspace)
            .and_then(|id| self.joined.workspace(id))
            .filter(|held| held.room().to_string() == workspace);
        read(joined.as_ref());
        Ok(())
    }

    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.profile.legacy_journal_root()
    }
}

impl WorkspaceMembership for Workspaces {
    fn with_workspace(&self, workspace: &RoomId, read: &mut dyn FnMut(&Workspace)) {
        let mut found = false;
        WorkspaceMembership::with_workspace(self.profile.as_ref(), workspace, &mut |held| {
            found = true;
            read(held);
        });
        if !found && let Some(held) = self.joined.workspace(*workspace) {
            read(&held);
        }
    }
}
