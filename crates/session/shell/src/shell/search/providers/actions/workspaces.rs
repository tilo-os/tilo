//! Workspace actions: rename, move and delete the workspace on screen, for
//! the palette, an agent or a script. The workspace settings' name field,
//! Move up and Move down, and Delete workspace call the same functions.

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

/// Longest workspace name a person or an agent may give, in characters.
const WORKSPACE_NAME_MAX_CHARS: usize = 120;

pub(super) const WORKSPACE_RENAME_PARAMS: &[ActionParam] = &[ActionParam::required(
    "name",
    ParamType::Text,
    "The new name: 1 to 120 characters without control characters.",
)];

pub(super) const WORKSPACE_MOVE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "position",
    ParamType::Integer,
    "Where the workspace on screen ends up in the list, counting from 1 at the top; past the \
     last workspace means the bottom.",
)];

/// What `workspace.delete` is told: the confirmation the shell door requires
/// of a destructive action.
pub(super) const WORKSPACE_DELETE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "confirm",
    ParamType::Boolean,
    "Confirm deleting the workspace on screen, here and on the person's other devices.",
)];

/// A workspace name as given, trimmed, or `None` when it is empty, too long
/// or holds control characters.
pub(super) fn valid_workspace_name(supplied: &str) -> Option<&str> {
    let name = supplied.trim();
    (!name.is_empty()
        && name.chars().count() <= WORKSPACE_NAME_MAX_CHARS
        && !supplied.chars().any(char::is_control))
    .then_some(name)
}

/// Rename the workspace on screen, as its settings' name field does.
pub(super) fn rename_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let name = valid_workspace_name(args.text("name").unwrap_or_default())
        .ok_or_else(|| ActionError::Explained(t("search-action-workspace-create-invalid-name")))?;
    if !state.may_rename_active_workspace() {
        return Err(ActionError::Refused(t(
            "search-action-workspace-rename-not-permitted",
        )));
    }
    state.rename_active_workspace(name);
    let renamed = state
        .tree()
        .desktop
        .active_workspace()
        .is_some_and(|workspace| workspace.name == name);
    if renamed {
        Ok(())
    } else {
        Err(ActionError::Refused(t(
            "search-action-workspace-rename-not-permitted",
        )))
    }
}

/// Move the workspace on screen to another place in the person's list: the
/// workspace settings' Move up and Move down, to any place at once.
pub(super) fn move_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let index = args
        .integer("position")
        .and_then(|position| usize::try_from(position).ok())
        .and_then(|position| position.checked_sub(1))
        .ok_or_else(|| ActionError::Refused(t("search-action-workspace-move-invalid-position")))?;
    state.move_active_workspace_to(index);
    Ok(())
}

/// Delete the workspace on screen, as its settings' Delete workspace does:
/// the person can undo it from the notice that follows, and their last
/// workspace stays.
pub(super) fn delete_workspace(
    state: &mut ChromeState,
    _args: &ActionArgs,
) -> Result<(), ActionError> {
    if state.tree().desktop.workspaces.len() < 2 {
        return Err(ActionError::Refused(t(
            "search-action-workspace-delete-last",
        )));
    }
    if state.delete_active_workspace() {
        Ok(())
    } else {
        Err(ActionError::Refused(t(
            "search-action-workspace-delete-failed",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chrome_with_room, invoke_shell_action};
    use super::super::{shell_action_contract, shell_action_kind};
    use crate::shell::chrome_state::Chrome;

    fn names(chrome: &Chrome) -> Vec<String> {
        chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.name.clone())
            .collect()
    }

    fn active_name(chrome: &Chrome) -> Option<String> {
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.name.clone())
    }

    /// What the workspace settings offer only to a pointer, an agent or a
    /// script can do too: rename the workspace on screen, and only to a
    /// name the settings' field would take.
    #[test]
    fn workspace_rename_names_the_workspace_on_screen() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        invoke_shell_action(
            &chrome,
            "workspace.rename",
            &serde_json::json!({ "name": "  Thesis  " }),
        )
        .expect("the workspace is renamed");
        assert_eq!(active_name(&chrome).as_deref(), Some("Thesis"));
        for refused in ["", "   ", "a\u{7}b", &"x".repeat(121)] {
            assert!(
                invoke_shell_action(
                    &chrome,
                    "workspace.rename",
                    &serde_json::json!({ "name": refused }),
                )
                .is_err(),
                "{refused:?} is not a name"
            );
        }
        assert_eq!(active_name(&chrome).as_deref(), Some("Thesis"));
    }

    /// A person who may not rename the workspace (a guest in someone else's)
    /// is refused, and the name stays.
    #[test]
    fn workspace_rename_refuses_a_person_who_may_not_rename_it() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        chrome
            .borrow()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("a workspace")
            .set_owner("someone-else");
        assert!(!chrome.borrow().may_rename_active_workspace());
        assert!(
            invoke_shell_action(
                &chrome,
                "workspace.rename",
                &serde_json::json!({ "name": "Mine now" }),
            )
            .is_err()
        );
        assert_eq!(active_name(&chrome).as_deref(), Some("Demo"));
    }

    /// The workspace on screen goes where it is told in the person's list,
    /// stays on screen, and past the end means the bottom.
    #[test]
    fn workspace_move_puts_the_workspace_on_screen_where_it_is_told() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        for name in ["Second", "Third"] {
            chrome.borrow_mut().add_workspace(name);
        }
        assert_eq!(names(&chrome), ["Demo", "Second", "Third"]);
        assert_eq!(active_name(&chrome).as_deref(), Some("Third"));
        invoke_shell_action(
            &chrome,
            "workspace.move",
            &serde_json::json!({ "position": 1 }),
        )
        .expect("moved to the top");
        assert_eq!(names(&chrome), ["Third", "Demo", "Second"]);
        assert_eq!(active_name(&chrome).as_deref(), Some("Third"));
        invoke_shell_action(
            &chrome,
            "workspace.move",
            &serde_json::json!({ "position": 9 }),
        )
        .expect("past the end means the bottom");
        assert_eq!(names(&chrome), ["Demo", "Second", "Third"]);
        assert!(
            invoke_shell_action(
                &chrome,
                "workspace.move",
                &serde_json::json!({ "position": 0 }),
            )
            .is_err(),
            "positions count from 1"
        );
        assert_eq!(names(&chrome), ["Demo", "Second", "Third"]);
    }

    /// Deleting asks for confirmation, takes the workspace on screen away
    /// with an undo, and keeps the person's last workspace.
    #[test]
    fn workspace_delete_needs_confirmation_and_keeps_the_last_workspace() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        chrome.borrow_mut().add_workspace("Scratch");
        assert_eq!(active_name(&chrome).as_deref(), Some("Scratch"));
        assert!(
            invoke_shell_action(&chrome, "workspace.delete", &serde_json::json!({})).is_err(),
            "a delete is confirmed"
        );
        assert_eq!(names(&chrome), ["Demo", "Scratch"]);
        invoke_shell_action(
            &chrome,
            "workspace.delete",
            &serde_json::json!({ "confirm": true }),
        )
        .expect("the workspace on screen is deleted");
        assert_eq!(names(&chrome), ["Demo"]);
        let undo = chrome
            .borrow()
            .notifications()
            .last()
            .map(|toast| toast.id)
            .expect("an undo notice");
        assert!(
            invoke_shell_action(
                &chrome,
                "workspace.delete",
                &serde_json::json!({ "confirm": true }),
            )
            .is_err(),
            "the last workspace stays"
        );
        assert_eq!(names(&chrome), ["Demo"]);
        assert!(chrome.borrow_mut().activate_notification(undo));
        assert_eq!(names(&chrome), ["Demo", "Scratch"]);
    }

    #[test]
    fn workspace_actions_say_what_they_change() {
        use tilo_actions::{ConfirmationPolicy, RiskLevel};
        for id in ["workspace.rename", "workspace.move"] {
            let contract = shell_action_contract(id);
            assert_eq!(contract.kind, tilo_actions::ActionKind::Undoable, "{id}");
            assert_eq!(
                shell_action_kind(id),
                tilo_actions::OperationKind::Document,
                "{id}"
            );
        }
        let delete = shell_action_contract("workspace.delete");
        assert_eq!(delete.risk, RiskLevel::Destructive);
        assert_eq!(delete.confirmation, ConfirmationPolicy::Confirm);
    }
}
