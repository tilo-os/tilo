//! Apps whose windows belong to the person and their device rather than to
//! any workspace: Personal Settings. Their windows open only in the
//! person's personal workspace, a workspace nobody else may read, so a
//! collaborator never receives one as a peer window.
//!
//! Opened while a shared workspace is shown, such a window goes to the
//! personal workspace and the view follows it there; it is never placed on,
//! moved onto or bound into a shared workspace's strip. Where a window lands
//! is decided in one place, [`crate::shell::tab_binding::place_tab`], which
//! asks [`WorkspaceTree::workspace_for_app`]; moves ask
//! [`WorkspaceTree::may_hold_app`].

use super::{RoomId, Workspace, WorkspaceId, WorkspaceTree};

/// The apps whose windows live only in the personal workspace.
const PERSONAL_APPS: &[&str] = &[crate::shell::display_control::SETTINGS_APP_ID];

/// Whether `app_id`'s windows belong to the person, not to a workspace.
#[must_use]
pub fn is_personal_app(app_id: &str) -> bool {
    PERSONAL_APPS
        .iter()
        .any(|personal| personal.eq_ignore_ascii_case(app_id))
}

impl WorkspaceTree {
    /// Whether `workspace` is personal: the person this device acts as
    /// owns it, and nobody but them may read it or is in it now. The
    /// person's other devices are the person; an agent the person admitted
    /// acts for them.
    #[must_use]
    pub fn is_personal_workspace(&self, workspace: &Workspace) -> bool {
        let Some(owner) = workspace.owner() else {
            return false;
        };
        let room = workspace.room();
        let owned_here = self.local_person.and_then(super::person_id_of).as_ref() == Some(owner)
            || *owner == super::local_user_id_for_room_prompt(&room);
        if !owned_here {
            return false;
        }
        let owner = Some(owner);
        let readers_are_owner = workspace
            .room_readers_at(tilo_clock::unix_now())
            .iter()
            .all(|(reader, _)| Some(reader) == owner);
        let nobody_else_here = workspace
            .collaborators
            .iter()
            .all(|person| person.own_device || Some(&person.id) == owner);
        readers_are_owner && nobody_else_here
    }

    /// The person's personal workspace: the shown one when it is personal,
    /// else the first personal one in the person's order.
    #[must_use]
    pub fn personal_workspace(&self) -> Option<WorkspaceId> {
        self.desktop
            .active_workspace()
            .filter(|workspace| self.is_personal_workspace(workspace))
            .or_else(|| {
                self.desktop
                    .workspaces
                    .iter()
                    .find(|workspace| self.is_personal_workspace(workspace))
            })
            .map(|workspace| workspace.id)
    }

    /// [`Self::personal_workspace`], made when the person has none: a new
    /// workspace placed on this device's host, owned by the person it acts
    /// as there ([`Self::create_hosted_workspace`]).
    pub fn personal_workspace_or_new(&mut self) -> WorkspaceId {
        if let Some(workspace) = self.personal_workspace() {
            return workspace;
        }
        let (made, _) =
            self.create_hosted_workspace(&crate::shell::i18n::t("workspace-personal-name"));
        self.claim_own_unhosted_rooms(super::local_user_id_for_room);
        made
    }

    /// The workspace a window of `app_id` asked for in `asked` opens in:
    /// `asked` itself, or for a personal app the personal workspace. `None`
    /// when a personal app has no personal workspace to open in.
    #[must_use]
    pub fn workspace_for_app(&self, app_id: &str, asked: WorkspaceId) -> Option<WorkspaceId> {
        if !is_personal_app(app_id) {
            return Some(asked);
        }
        if self
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.id == asked && self.is_personal_workspace(workspace))
        {
            return Some(asked);
        }
        self.personal_workspace()
    }

    /// [`Self::workspace_for_app`], making the personal workspace when a
    /// personal app has none to open in.
    pub fn home_for_app(&mut self, app_id: &str, asked: WorkspaceId) -> WorkspaceId {
        self.workspace_for_app(app_id, asked)
            .unwrap_or_else(|| self.personal_workspace_or_new())
    }

    /// The room a launch of `app_id` asked from `room` runs in: `room`
    /// itself, or for a personal app the personal workspace's
    /// ([`Self::home_for_app`]).
    pub fn room_for_app(&mut self, app_id: &str, room: RoomId) -> RoomId {
        if !is_personal_app(app_id) {
            return room;
        }
        let home = match self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| workspace.id)
        {
            Some(asked) => self.home_for_app(app_id, asked),
            None => self.personal_workspace_or_new(),
        };
        self.room_of(home).unwrap_or(room)
    }

    /// Whether a window of `app_id` may be on `workspace`'s strips.
    #[must_use]
    pub fn may_hold_app(&self, workspace: WorkspaceId, app_id: &str) -> bool {
        !is_personal_app(app_id)
            || self
                .desktop
                .workspaces
                .iter()
                .any(|candidate| candidate.id == workspace && self.is_personal_workspace(candidate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = "so.tilo.settings";

    /// A personal workspace and a shared one, the shared one shown.
    fn tree() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let mut tree = WorkspaceTree::default();
        let (personal, _) = tree.create_hosted_workspace("Mine");
        let (shared, _) = tree.create_hosted_workspace("Together");
        tree.own_workspaces_for_test(super::super::local_user_id_for_room);
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == shared)
            .expect("the shared workspace")
            .admit("bob", tilo_action_policy::Role::Editor);
        tree.set_active(shared);
        (tree, personal, shared)
    }

    #[test]
    fn a_workspace_anyone_else_may_read_is_not_personal() {
        let (tree, personal, shared) = tree();
        let is_personal = |id| {
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == id)
                .is_some_and(|workspace| tree.is_personal_workspace(workspace))
        };
        assert!(is_personal(personal));
        assert!(!is_personal(shared));
        assert_eq!(tree.personal_workspace(), Some(personal));
    }

    #[test]
    fn settings_asked_from_a_shared_workspace_runs_in_the_personal_one() {
        let (mut tree, personal, shared) = tree();
        let personal_room = tree.room_of(personal).expect("a room");
        let shared_room = tree.room_of(shared).expect("a room");
        assert_eq!(tree.workspace_for_app(SETTINGS, shared), Some(personal));
        assert_eq!(tree.room_for_app(SETTINGS, shared_room), personal_room);
        assert_eq!(tree.room_for_app(SETTINGS, personal_room), personal_room);
        assert!(!tree.may_hold_app(shared, SETTINGS));
        assert!(tree.may_hold_app(personal, SETTINGS));

        // Every other app opens where it was asked.
        assert_eq!(
            tree.workspace_for_app("so.tilo.notes", shared),
            Some(shared)
        );
        assert_eq!(tree.room_for_app("so.tilo.notes", shared_room), shared_room);
        assert!(tree.may_hold_app(shared, "so.tilo.notes"));
    }

    #[test]
    fn a_shared_workspace_shown_does_not_become_the_personal_one() {
        let (mut tree, personal, shared) = tree();
        // A second personal workspace, shown: Settings opens right there.
        let (other, _) = tree.create_hosted_workspace("Also mine");
        tree.own_workspaces_for_test(super::super::local_user_id_for_room);
        tree.set_active(other);
        assert_eq!(tree.workspace_for_app(SETTINGS, other), Some(other));
        assert_eq!(tree.workspace_for_app(SETTINGS, shared), Some(other));
        tree.set_active(shared);
        assert_eq!(tree.workspace_for_app(SETTINGS, shared), Some(personal));
    }
}
