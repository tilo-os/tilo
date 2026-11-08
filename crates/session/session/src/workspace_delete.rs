//! Deleting a workspace: one of the person's own, alone and held by no other device of theirs, so
//! deleting it here deletes it with everything in it.
//! The person answers a question first, on a small dialog of its own
//! ([`tilo_sheets::delete_workspace`]) wherever deleting was asked for — the workspace menu, the
//! workspace settings, an agent's action through the desk. Only their yes takes the workspace off
//! the desk, which is what the person's list follows, and then out of this device's record.

use serde_json::{Map, Value};
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_desktop_surface::notifications::{Notification, NotificationIconKind};
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
        if let Some(engine) = &self.engine
            && let Err(error) = engine.delete(workspace)
        {
            tracing::warn!(%error, %workspace, "a deleted workspace stays in this device's record");
        }
        let mut notice = Notification::new(
            tilo_desktop_surface::i18n::t_args(
                "session-deleted-workspace",
                &[("name", name.into())],
            ),
            tilo_desktop_surface::i18n::t("session-deleted-workspace-body"),
        );
        notice.icon_kind = NotificationIconKind::Workspace;
        self.notices.post(notice);
    }
}
