//! What the person at this device may change of the shell's workspaces and strips, as the shell's
//! host answers it: the desktop's one policy over the person, who owns this desktop, and the
//! device's daemon for which workspaces it holds and lets go of.

use std::cell::RefCell;
use std::sync::Arc;

use tilo_action_policy::Role;
use tilo_desktop_model::commands::{DesktopCommand, Requester, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{StripId, WorkspaceId};
use tilo_session_core::desktop::host::contract::DesktopRules;
use tilo_session_core::desktop::i18n::t;
use tilo_session_core::desktop::state::DesktopHandle;

/// The session's answers to what the desktop asks before it changes anything.
pub(crate) struct SessionRules {
    handle: DesktopHandle,
    /// Who this desktop acts for, and changes the workspace as.
    person: String,
    /// How the person's workspaces stand, as the device's daemon publishes them: what says
    /// whether a workspace may be deleted.
    view: Option<Arc<crate::workspace_view::WorkspaceView>>,
    /// Who made each strip: what its maker may change of it.
    authors: RefCell<Authors>,
}

impl SessionRules {
    pub(crate) fn new(
        handle: DesktopHandle,
        person: String,
        view: Option<Arc<crate::workspace_view::WorkspaceView>>,
    ) -> Self {
        Self {
            handle,
            person,
            view,
            authors: RefCell::new(Authors::new()),
        }
    }

    /// The person at this device owns its desktop: the desktop's one policy answers what they may
    /// change, as it answers a workspace's owner.
    fn requester(&self) -> Requester<'_> {
        Requester {
            person: &self.person,
            role: Some(Role::Owner),
            members: None,
        }
    }
}

impl DesktopRules for SessionRules {
    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let authors = self.authors.borrow();
        let requester = self.requester();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            may_command(&requester, &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        may_create_workspace(&self.requester()).map_err(|refusal| refusal.to_string())
    }

    fn may_delete_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        let Some(view) = &self.view else {
            return Err(t("desktop-cannot-delete-workspace"));
        };
        let id = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .map(tilo_desktop_model::Workspace::id)
            })
            .ok_or_else(|| t("desktop-workspace-gone"))?;
        if view.may_delete(id) {
            Ok(())
        } else {
            Err(t("desktop-workspace-stays"))
        }
    }

    /// A workspace may be branched by whoever may make a workspace and reads all of it: the person
    /// is in it here and not removed. One this device holds no governance of is the shell's own,
    /// and the person's unless it names someone else as its owner.
    fn may_branch_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.may_create_workspace()?;
        let (id, owner) = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .map(|held| (held.id(), held.owner().cloned()))
            })
            .ok_or_else(|| t("desktop-workspace-gone"))?;
        let reads = match &self.view {
            Some(view) if view.holds(id) => view.reads_whole(id),
            _ => owner.is_none_or(|owner| owner == self.person),
        };
        if reads {
            Ok(())
        } else {
            Err(t("desktop-cannot-branch-workspace"))
        }
    }

    /// A workspace that exists comes on the shell once the device's daemon holds it: entered
    /// from the person's list or joined by an invitation, the session puts it there itself. One
    /// it holds nothing of would be founded here afresh under the existing workspace's id.
    ///
    /// A session that reaches no daemon holds none of a workspace it joined and reaches no host to
    /// hand it one: it shows the workspace with what this computer keeps of
    /// it, as a join here has always left it, and nothing of it leaves this machine.
    fn may_open_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        match &self.view {
            Some(view) if !view.holds(workspace) => Err(t("desktop-workspace-not-held")),
            _ => Ok(()),
        }
    }

    fn made_strip(&self, strip: StripId) {
        self.authors
            .borrow_mut()
            .insert(strip.0, self.person.clone());
    }
}
