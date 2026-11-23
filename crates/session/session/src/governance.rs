//! The desktop's half of a workspace's governance: the name, people and saved shortcuts the
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

/// What a change to a workspace is written through: the shell, the person making it, this
/// device's engine and where the loop takes in what the workspace answered.
pub(crate) struct Governing<'a> {
    pub(crate) handle: &'a tilo_desktop_surface::DesktopHandle,
    pub(crate) person: &'a tilo_desktop_model::UserId,
    pub(crate) engine: Option<&'a std::sync::Arc<crate::engine::Engine>>,
    pub(crate) landings: &'a crate::landing::Landings,
}

/// Make `edit` to `workspace` as the person, where the workspace keeps it: the workspace's
/// governance first, signed with this device's key, so every member admits it as it admits every
/// change its people make, and the shell takes the change only once the workspace took it. A
/// workspace whose governance this device does not hold has only the shell. `landed` hears whether
/// the change stands. Whether `edit` changed anything, or why it was refused at once.
pub(crate) fn govern(
    at: &Governing<'_>,
    workspace_id: WorkspaceId,
    edit: impl Fn(
        &mut Workspace,
        &tilo_desktop_model::UserId,
    )
        -> Result<tilo_desktop::workspace_settings::PolicyEdit, tilo_action_policy::Refusal>
    + 'static,
    landed: impl FnOnce(Result<(), String>) + 'static,
) -> Result<bool, tilo_action_policy::Refusal> {
    let mut tree = at.handle.read(Clone::clone);
    let Some(workspace) = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == workspace_id)
    else {
        return Ok(false);
    };
    let changed = edit(workspace, at.person)?;
    if changed.is_empty() {
        return Ok(false);
    }
    let Some(engine) = at
        .engine
        .filter(|engine| engine.policy(&workspace_id.to_string()).is_ok())
        .cloned()
    else {
        at.handle.replace_tree(tree);
        landed(Ok(()));
        return Ok(true);
    };
    let writes =
        crate::membership::Membership::of(&engine, workspace_id).changes(workspace, &changed);
    let written = crate::landing::off_loop(async move {
        crate::membership::Membership::of(&engine, workspace_id)
            .write(&writes)
            .await
    });
    let (handle, person) = (at.handle.clone(), at.person.clone());
    at.landings.then(written, move |written| {
        let written = written
            .unwrap_or_else(|| Err("the change was given up before the workspace answered".into()));
        if written.is_ok() {
            // The shell takes the change as it stands now, which moved while the workspace
            // answered.
            let mut tree = handle.read(Clone::clone);
            if let Some(workspace) = tree
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id() == workspace_id)
                && edit(workspace, &person).is_ok_and(|changed| !changed.is_empty())
            {
                handle.replace_tree(tree);
            }
        }
        landed(written);
    });
    Ok(true)
}

impl crate::Shared {
    /// Who each workspace on the shell has in it and which shortcuts it saved, as the workspace's
    /// own governance here says: who it was shared with, in what part, when each was admitted, and
    /// its shortcuts as each saver's own device wrote them. The shell keeps no roster of its own —
    /// its workspaces carry the governance's people and shortcuts as a projection, which is what
    /// the People and Shortcuts surfaces, the broker, the trigger host and every other reader of
    /// them read.
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
        // Who can be found in each workspace, as its governance names them.
        self.search.set_members(
            fed.1
                .iter()
                .filter_map(|workspace| {
                    let workspace = workspace.to_string();
                    let people = engine.people(&workspace).ok()?;
                    Some((workspace, people))
                })
                .collect(),
        );
        *self.membership_fed.borrow_mut() = Some(fed);
        let session = Session(&self.handle);
        let projected = session.edit(&mut |workspaces| {
            let mut changed = false;
            for workspace in workspaces {
                // The workspace's one name, which the shell keeps none of its own of.
                if let Some(name) = self.governing_name(workspace.id()) {
                    changed |= workspace.named_as(&name);
                }
                let Ok(policy) = engine.policy(&workspace.id().to_string()) else {
                    // A workspace whose governance this device does not hold keeps the people
                    // and shortcuts the shell has of it.
                    continue;
                };
                changed |= membership::project(workspace, &policy);
                if let Ok(people) = engine.people(&workspace.id().to_string()) {
                    changed |= membership::project_names(workspace, &people);
                }
                let Ok((shortcuts, sandbox)) =
                    engine.owner().with_current(workspace.id(), |view| {
                        Ok((
                            view.peer().workspace().workspace_shortcuts(),
                            view.peer().governance().governed_sandbox().ok(),
                        ))
                    })
                else {
                    continue;
                };
                if let Some(sandbox) = sandbox {
                    changed |= project_sandbox(workspace, sandbox);
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

/// What the workspace's governance lets its apps reach, on to the shell's `workspace`: an app one
/// member allowed starts on every member's device. A capability that lasts only while this
/// session does is this device's alone and stays; one the governance no longer holds was taken
/// back, so it goes. Whether the shell's workspace changed.
fn project_sandbox(
    workspace: &mut Workspace,
    mut governed: Vec<collab_types::caps::CapOverride>,
) -> bool {
    governed.extend(
        workspace
            .sandbox
            .iter()
            .filter(|capability| !capability.is_durable())
            .cloned(),
    );
    governed.sort_by(|a, b| a.id.cmp(&b.id));
    let mut held = workspace.sandbox.clone();
    held.sort_by(|a, b| a.id.cmp(&b.id));
    if held == governed {
        return false;
    }
    workspace.sandbox = governed;
    true
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
