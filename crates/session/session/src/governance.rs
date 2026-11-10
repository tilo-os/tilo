//! The desktop's half of a workspace's governance: the people and bots the desk's workspaces show,
//! projected from the governance this device holds, and the desk the grants are written into.

use tilo_desktop_model::Workspace;
use tilo_session_client::membership::{self, Workspaces};

/// The desk's own workspaces, as a workspace's governance writes into them.
pub(crate) struct Desk<'a>(pub(crate) &'a tilo_desktop_surface::DesktopHandle);

impl Workspaces for Desk<'_> {
    fn edit(
        &self,
        edit: &mut dyn FnMut(&mut [Workspace]) -> Result<bool, String>,
    ) -> Result<(), String> {
        let mut tree = self.0.read(Clone::clone);
        let changed = edit(&mut tree.workspaces)?;
        if changed {
            self.0.replace_tree(tree);
        }
        Ok(())
    }
}

impl crate::Shared {
    /// Who each workspace on the desk has in it and which bots it runs, as the workspace's own
    /// governance here says: who it was shared with, in what part, when each was admitted, and the
    /// bots it runs or asks its host for. The desk keeps no roster of its own — its workspaces
    /// carry the governance's people and bots as a projection, which is what the People and Agents
    /// tabs, the broker and every other reader of the roster read. Nothing of a workspace's state
    /// reaches this device without the engine admitting it, so a revision that has not moved has
    /// nothing to show.
    pub(crate) fn project_governance(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let revision = engine.owner().revision();
        if self.membership_fed.get() == Some(revision) {
            return;
        }
        self.membership_fed.set(Some(revision));
        let mut tree = self.handle.read(Clone::clone);
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let Ok(policy) = engine.policy(&workspace.id().to_string()) else {
                // A workspace whose governance this device does not hold keeps the people and bots
                // the desk has of it.
                continue;
            };
            changed |= membership::project(workspace, &policy);
            let Ok(bots) = engine.owner().with_current(workspace.id(), |view| {
                Ok(view.peer().governance().workspace_bots())
            }) else {
                continue;
            };
            if workspace.bots != bots {
                workspace.bots = bots;
                changed = true;
            }
        }
        if changed {
            self.handle.replace_tree(tree);
        }
    }
}
#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
