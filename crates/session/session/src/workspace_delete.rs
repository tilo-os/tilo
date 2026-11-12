//! Deleting a workspace: one of the person's own, alone and held by no other device of theirs, so
//! deleting it here deletes it with everything in it.
//! The person answers a question first, on a small dialog of its own
//! ([`tilo_sheets::delete_workspace`]) wherever deleting was asked for — the workspace menu, the
//! workspace settings, an agent's action through the desk. Only their yes takes the workspace off
//! the desk, which is what the person's list follows, and then out of this device's record.
//! A workspace its owner deleted somewhere else leaves this device the same way, and the person is
//! told who deleted it ([`Shared::follow_owner_deletions`]).

use serde_json::{Map, Value};
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_sheets::Seat as _;
use tilo_sheets::delete_workspace::DeleteWorkspaceEffect;

use crate::Shared;

impl Shared {
    /// Ask the person whether to delete the workspace `slot`, called `name`:
    /// the question stands in its own dialog over what they are looking at,
    /// and nothing of the workspace goes until they say yes.
    pub(crate) fn ask_to_delete_workspace(&self, slot: WorkspaceSlot, name: &str) {
        let Some(seat) = self.delete_workspace.as_ref() else {
            tracing::warn!(
                slot = slot.0,
                "no delete question is mounted: nothing is deleted"
            );
            return;
        };
        *self.delete_workspace_ask.borrow_mut() = Some(slot);
        seat.show(name.to_owned());
        seat.presentation().open(false);
    }

    /// Ask the desk to delete the workspace in front: the desk asks the person first, and only
    /// their answer deletes it.
    pub(crate) fn ask_desk_to_delete(&self) {
        if let Err(refusal) = self.ask_of_desk("delete-workspace", &Map::new()) {
            self.handle.acknowledge(refusal);
        }
    }

    /// Carry out the person's answer on the delete question: on yes the desk deletes the workspace,
    /// which is the only thing that deletes, and on keep nothing happens at all.
    pub(crate) fn follow_delete_workspace(&self) {
        let Some(seat) = self.delete_workspace.as_ref() else {
            return;
        };
        for effect in seat.take_answers() {
            let Some(slot) = self.delete_workspace_ask.borrow_mut().take() else {
                continue;
            };
            seat.presentation().close();
            seat.presentation().gone();
            if effect == DeleteWorkspaceEffect::Delete {
                self.delete_workspace(slot);
            }
        }
    }

    /// Delete the workspace `slot` through the desk's own published action,
    /// as the person's answer to the question: what may be deleted, and the
    /// removal, are the desk's, and so is telling the session it happened
    /// ([`Self::deleted_workspace`]).
    fn delete_workspace(&self, slot: WorkspaceSlot) {
        let args = Map::from_iter([
            ("workspace".to_owned(), Value::from(slot.0.to_string())),
            // The person said yes to the question deleting asks.
            ("confirmed".to_owned(), Value::from(true)),
        ]);
        // Off the desk first: a desk that refused leaves the workspace whole,
        // so nothing of it is gone while it still stands.
        if let Err(refusal) = self.ask_of_desk("delete-workspace", &args) {
            self.handle.acknowledge(refusal);
        }
    }

    /// The desk deleted the workspace `workspace` , called `name` : what is left of it in this
    /// device's record goes with it, and the person is told. The desk asks this of whoever deleted
    /// it — the menu, the settings, an agent invoking the action — so a workspace deleted anywhere
    /// is deleted everywhere it was this device's.
    pub(crate) fn deleted_workspace(&self, workspace: WorkspaceId, name: &str) {
        if let Some(seat) = &self.delete_workspace {
            seat.presentation().close();
            seat.presentation().gone();
        }
        self.delete_workspace_ask.borrow_mut().take();
        self.forget_deleted(workspace);
        crate::directory::workspace_notice(
            &*self.telling(),
            (
                "session-deleted-workspace",
                "session-deleted-workspace-body",
            ),
            name,
        );
    }

    /// Take each workspace whose host said its owner deleted it off this desk and out of this
    /// device's record, and tell the person: someone else deleted it, so it is news to them.
    pub(crate) fn follow_owner_deletions(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        for workspace in sync.take_deleted() {
            tracing::info!(%workspace, "a workspace its owner deleted leaves this device");
            let name = self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .map(|held| held.name.clone())
            });
            // Told once: a word the host repeats finds nothing left to take away.
            let gone = self.take_off_desk(workspace) | self.forget_deleted(workspace);
            if gone && let Some(name) = name {
                crate::directory::workspace_notice(
                    &*self.telling(),
                    (
                        "session-owner-deleted-workspace",
                        "session-owner-deleted-workspace-body",
                    ),
                    &name,
                );
            }
        }
    }

    /// What is left of the deleted workspace `workspace` in this device's record goes; whether the
    /// record held it.
    fn forget_deleted(&self, workspace: WorkspaceId) -> bool {
        let Some(engine) = &self.engine else {
            return false;
        };
        engine.delete(workspace).unwrap_or_else(|error| {
            tracing::warn!(%error, %workspace, "a deleted workspace stays in this device's record");
            false
        })
    }

    /// Take `workspace`, which is gone already, off the desk; whether it was taken. Nobody is asked
    /// and nobody refuses: the person goes to the workspace beside it when they were in it, and
    /// only a desk's one workspace stays, as the desk is never left with none.
    fn take_off_desk(&self, workspace: WorkspaceId) -> bool {
        let mut tree = self.handle.read(Clone::clone);
        let Some(at) = tree
            .workspaces
            .iter()
            .position(|held| held.id() == workspace)
        else {
            return false;
        };
        if tree.workspaces.len() == 1 {
            return false;
        }
        let gone = tree.workspaces.remove(at);
        if gone.slot == tree.active
            && let Some(next) = tree
                .workspaces
                .get(at.min(tree.workspaces.len().saturating_sub(1)))
        {
            tree.active = next.slot;
        }
        tree.active_strip.remove(&gone.slot);
        for strip in &gone.strips {
            tree.active_group.remove(&strip.id);
        }
        self.handle.replace_tree(tree);
        true
    }
}
