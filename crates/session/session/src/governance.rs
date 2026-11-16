//! The desktop's half of a workspace's governance: the name, people, bots and saved shortcuts the
//! shell's workspaces show, projected from the governance this device holds, and the shell the
//! grants are written into.

use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_session_client::membership::{self, Workspaces};

/// The shell's own workspaces, as a workspace's governance writes into them.
pub(crate) struct Session<'a>(pub(crate) &'a tilo_desktop_surface::DesktopHandle);

impl Workspaces for Session<'_> {
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
    /// Who each workspace on the shell has in it, which bots it runs and which shortcuts it saved,
    /// as the workspace's own governance here says: who it was shared with, in what part, when each
    /// was admitted, the bots it runs or asks its host for, and its shortcuts as each saver's own
    /// device wrote them. The shell keeps no roster of its own — its workspaces carry the
    /// governance's people, bots and shortcuts as a projection, which is what the People, Agents and
    /// Shortcuts surfaces, the broker, the trigger host and every other reader of them read.
    /// Nothing of a workspace's state reaches this device without the engine admitting it, so a
    /// revision that has not moved has nothing to show. A workspace the shell shows that it did not
    /// show before is projected all the same: what it holds was here before the shell showed it,
    /// as a workspace a person kept or one they just joined is.
    pub(crate) fn project_governance(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let shown: Vec<WorkspaceId> = self
            .handle
            .read(|tree| tree.workspaces.iter().map(Workspace::id).collect());
        let fed = (engine.owner().revision(), shown);
        if self.membership_fed.borrow().as_ref() == Some(&fed) {
            return;
        }
        *self.membership_fed.borrow_mut() = Some(fed);
        let session = Session(&self.handle);
        let projected = session.edit(&mut |workspaces| {
            let mut changed = false;
            for workspace in workspaces {
                // The workspace's one name, which the shell keeps none of its own of.
                if let Ok(Some(name)) = engine.owner().with_current(workspace.id(), |view| {
                    Ok(view.peer().governance().workspace_name())
                }) {
                    changed |= workspace.named_as(&name);
                }
                let Ok(policy) = engine.policy(&workspace.id().to_string()) else {
                    // A workspace whose governance this device does not hold keeps the people,
                    // bots and shortcuts the shell has of it.
                    continue;
                };
                changed |= membership::project(workspace, &policy);
                let Ok((bots, shortcuts)) = engine.owner().with_current(workspace.id(), |view| {
                    Ok((
                        view.peer().governance().workspace_bots(),
                        view.peer().workspace().workspace_shortcuts(),
                    ))
                }) else {
                    continue;
                };
                if workspace.bots != bots {
                    workspace.bots = bots;
                    changed = true;
                }
                // A shortcut deleted in the workspace is absent from what it holds, so the shell's
                // list is the workspace's, never the two merged.
                if workspace.shortcuts != shortcuts {
                    workspace.shortcuts = shortcuts;
                    changed = true;
                }
            }
            Ok(changed)
        });
        if let Err(reason) = projected {
            tracing::warn!(%reason, "the session's people were not projected");
        }
    }
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
