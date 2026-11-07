//! Deleting a workspace: a workspace of the person's, alone and held by no
//! other device of theirs, so deleting it here deletes it with everything in
//! it.
//!
//! The person is asked first, on a banner whose two answers are its host
//! actions ([`Shared::answer_delete_workspace`]). Confirming takes the
//! workspace off the desk, which is what the person's list follows, and then
//! out of this device's record.

use serde_json::{Map, Value};
use tilo_desktop_model::WorkspaceId;
use tilo_desktop_surface::{
    host_action_key,
    notifications::{
        Addressed, Notification, NotificationIconKind, NotificationSeverity, PERSISTENT_DISMISS,
    },
};

use crate::Shared;

/// What a delete banner's answers are keyed by: the workspace they are about
/// and which answer each one is.
const PREFIX: &str = "workspace-delete:";

/// The banner button that deletes `workspace`, or keeps it.
fn answer_key(workspace: WorkspaceId, delete: bool) -> String {
    format!(
        "{PREFIX}{workspace}:{}",
        if delete { "delete" } else { "keep" }
    )
}

/// The workspace and the answer `choice` carries, when it is one of this
/// module's banner's.
fn answer(choice: &str) -> Option<(WorkspaceId, bool)> {
    let (workspace, verdict) = choice.strip_prefix(PREFIX)?.rsplit_once(':')?;
    Some((
        WorkspaceId::parse(workspace)?,
        match verdict {
            "delete" => true,
            "keep" => false,
            _ => return None,
        },
    ))
}

impl Shared {
    /// Ask the person whether to delete the workspace shown: what the
    /// workspace menu's own row does, and the only way a workspace goes.
    pub(crate) fn ask_delete_workspace(&self) {
        let Some((workspace, name)) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| (held.id(), held.name.clone()))
        }) else {
            return;
        };
        let mut notice = Notification::new(
            tilo_desktop_surface::i18n::t_args(
                "session-delete-workspace",
                &[("name", name.into())],
            ),
            tilo_desktop_surface::i18n::t("session-delete-workspace-body"),
        );
        notice.actions = vec![
            (
                host_action_key(&answer_key(workspace, true)),
                tilo_desktop_surface::i18n::t("session-delete-workspace-confirm"),
            ),
            (
                host_action_key(&answer_key(workspace, false)),
                tilo_desktop_surface::i18n::t("session-delete-workspace-cancel"),
            ),
        ];
        notice.icon_kind = NotificationIconKind::Workspace;
        notice.severity = NotificationSeverity::Warning;
        notice.addressed = Some(Addressed::Ask);
        notice.dismiss_after = PERSISTENT_DISMISS;
        self.notices.post(notice);
    }

    /// The person chose the button of the delete banner `choice` carries:
    /// both answers go, and on delete the workspace goes with everything in
    /// it. Whether `choice` was one of this module's answers; one about a
    /// workspace that has since gone from the desk deletes nothing.
    pub(crate) fn answer_delete_workspace(&self, choice: &str) -> bool {
        let Some((workspace, delete)) = answer(choice) else {
            return false;
        };
        for verdict in [true, false] {
            self.notices
                .withdraw_host_action(&host_action_key(&answer_key(workspace, verdict)));
        }
        if delete {
            self.delete_workspace(workspace);
        }
        true
    }

    /// Delete the workspace `workspace` through the desk's own published
    /// action: what may be deleted, and the removal, are the desk's, and so
    /// is telling the session it happened ([`Self::deleted_workspace`]).
    fn delete_workspace(&self, workspace: WorkspaceId) {
        let Some(slot) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id() == workspace)
                .map(|held| held.slot)
        }) else {
            return;
        };
        let args = Map::from_iter([("workspace".to_owned(), Value::from(slot.0.to_string()))]);
        // Off the desk first: a desk that refused leaves the workspace whole,
        // so nothing of it is gone while it still stands.
        if let Err(error) = self.ask_of_desk("delete-workspace", &args) {
            self.handle.acknowledge(error);
        }
    }

    /// The desk deleted the workspace `workspace`, called `name`: what is
    /// left of it in this device's record goes with it, and the person is
    /// told. The desk asks this of whoever deleted it — the menu, the
    /// banner's answer, an agent invoking the action — so a workspace deleted
    /// anywhere is deleted everywhere it was this device's.
    pub(crate) fn deleted_workspace(&self, workspace: WorkspaceId, name: &str) {
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

#[cfg(test)]
mod tests {
    use super::{answer, answer_key};
    use tilo_desktop_model::WorkspaceId;

    #[test]
    fn a_banner_button_names_the_workspace_and_its_answer() {
        let workspace = WorkspaceId::new();
        assert_eq!(
            answer(&answer_key(workspace, true)),
            Some((workspace, true))
        );
        assert_eq!(
            answer(&answer_key(workspace, false)),
            Some((workspace, false))
        );
        assert_eq!(answer("workspace-delete:nonsense:delete"), None);
        assert_eq!(answer("leave-workspace"), None);
    }
}
