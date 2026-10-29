//! Every room this profile takes part in, as the brokers ask about them: the
//! profile's own workspace, whose record holds what this profile keeps of
//! it (the person's grants to apps, effect records), and the rooms it
//! joined, the profile's own room included once its host placed it.
use super::{joined::Joined, workspace::ProfileWorkspace};
use std::sync::Arc;
use tilo_action_broker::services::WorkspaceAuthority;
use tilo_desktop_model::{DesktopTree, RoomId, Workspace};
use tilo_identity_host::broker::WorkspaceMembership;

pub(super) struct Rooms {
    pub(super) profile: Arc<ProfileWorkspace>,
    pub(super) joined: Arc<Joined>,
}

impl Rooms {
    pub(super) fn new(profile: &Arc<ProfileWorkspace>, joined: &Arc<Joined>) -> Self {
        Self {
            profile: Arc::clone(profile),
            joined: Arc::clone(joined),
        }
    }
}

impl WorkspaceAuthority for Rooms {
    /// The profile's own workspaces, then each joined room as its
    /// governance describes it. Joined rooms are never written to the
    /// profile's workspace file, so a profile that joined any room reads
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

    /// The profile's own room first, as in the merged tree; otherwise only
    /// the requested joined room is built.
    fn with_room(
        &self,
        room: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        let mut read = Some(read);
        self.profile.with_room(room, &mut |workspace| {
            if workspace.is_some()
                && let Some(read) = read.take()
            {
                read(workspace);
            }
        })?;
        let Some(read) = read else {
            return Ok(());
        };
        let joined = RoomId::parse(room)
            .and_then(|id| self.joined.workspace(id))
            .filter(|workspace| workspace.room().to_string() == room);
        read(joined.as_ref());
        Ok(())
    }

    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.profile.legacy_journal_root()
    }
}

impl WorkspaceMembership for Rooms {
    fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
        let mut found = false;
        self.profile.with_workspace(room, &mut |workspace| {
            found = true;
            read(workspace);
        });
        if !found && let Some(workspace) = self.joined.workspace(*room) {
            read(&workspace);
        }
    }
}
