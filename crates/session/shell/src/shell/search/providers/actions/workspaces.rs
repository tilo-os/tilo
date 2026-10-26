//! Workspace actions the workspace's settings offer by click: delete the
//! workspace on screen and move it earlier or later in the person's list,
//! for the palette, an agent, a script or a screen reader.

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

use super::active_workspace_id;

/// What `workspace.delete` may be told: the person's confirmation, which
/// the palette asks for as one more step.
pub(super) const WORKSPACE_DELETE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "confirm",
    ParamType::Boolean,
    "Delete the workspace on screen, here and on the person's other devices. Without it nothing \
     is deleted and the person is asked first.",
)];

/// [`WORKSPACE_DELETE_PARAMS`], the confirmation reading as what it does.
pub(super) fn workspace_delete_params() -> Vec<ActionParam> {
    WORKSPACE_DELETE_PARAMS
        .iter()
        .cloned()
        .map(|param| {
            param.with_labelled_choices([("true", t("search-action-workspace-delete-confirm"))])
        })
        .collect()
}

/// Delete the workspace on screen, as its settings' Delete workspace does,
/// once the person has confirmed it. The last workspace stays.
pub(super) fn delete_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let count = state.tree().desktop.workspaces.len();
    if count < 2 {
        return Err(ActionError::Explained(t(
            "search-action-workspace-delete-last",
        )));
    }
    if args.boolean("confirm") != Some(true) {
        return Err(ActionError::ConfirmationRequired);
    }
    if !state.delete_active_workspace() {
        return Err(ActionError::Explained(t(
            "search-action-workspace-delete-failed",
        )));
    }
    // Its settings showed the workspace that is gone.
    state.overlays.ws_settings = false;
    Ok(())
}

/// Move the workspace on screen one place earlier (`step < 0`) or later
/// in the person's list, as its settings' Move up and Move down do.
pub(super) fn move_workspace(state: &mut ChromeState, step: i32) -> Result<(), ActionError> {
    active_workspace_id(state)?;
    if state.move_active_workspace(step) {
        Ok(())
    } else {
        Err(ActionError::Explained(t(if step < 0 {
            "search-action-workspace-move-first"
        } else {
            "search-action-workspace-move-last"
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{action_results, chrome_with_room, invoke_shell_action};
    use super::super::{ActionsProvider, shell_action_contract};
    use crate::shell::chrome_state::Chrome;
    use crate::shell::search::action_api::{ActionArgs, ActionError};
    use crate::shell::state::FreshId as _;
    use automerge::AutoCommit;

    /// The workspace on screen and a second one after it, the second shown.
    /// The first one's room is placed on this device, whose native owner
    /// is open: a workspace leaves with its room through that owner. The
    /// room stays placed while the returned guard lives.
    fn chrome_with_two_workspaces() -> (
        Chrome,
        crate::shell::state::WorkspaceId,
        crate::shell::test_support::OwnHostedRoom,
    ) {
        use crate::shell::state::{Workspace, WorkspaceId};
        let chrome = chrome_with_room();
        let hosted = crate::shell::test_support::host_active_room(&chrome.borrow().tree_handle());
        let second = WorkspaceId::new();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let mut workspace = Workspace::new(second, "Second");
            workspace.set_owner("shell-fixture");
            tree.desktop.workspaces.push(workspace);
            tree.desktop.active = second;
        }
        (chrome, second, hosted)
    }

    fn workspace_names(chrome: &Chrome) -> Vec<String> {
        chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.name.clone())
            .collect()
    }

    /// Deleting a workspace is an action, asked first: the palette gets one
    /// more step to confirm it, an agent must say `confirm`, and the last
    /// workspace stays.
    #[test]
    fn deleting_a_workspace_asks_first_and_keeps_the_last_one() {
        let (chrome, _, _hosted) = chrome_with_two_workspaces();
        // The first workspace is named as its room is.
        let first = workspace_names(&chrome)[0].clone();
        let provider = ActionsProvider::new();
        let row = action_results(&provider, &chrome)
            .into_iter()
            .find(|row| row.id == "workspace.delete")
            .expect("workspace.delete is published");
        let confirm = row
            .params
            .iter()
            .find(|param| param.name == "confirm")
            .expect("it takes the person's confirmation");
        assert!(
            !confirm.required,
            "the palette asks for it as one more step"
        );
        assert_eq!(confirm.choice_label("true"), Some("Delete workspace"));
        let contract = row.contract.clone().expect("a contract");
        assert_eq!(contract.risk, tilo_actions::RiskLevel::Destructive);
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            contract.validate_declaration().is_empty(),
            "{:?}",
            contract.validate_declaration()
        );

        // As the palette runs it: nothing is deleted until the person says so.
        let mut document = AutoCommit::new();
        let unconfirmed = (row.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        );
        assert_eq!(unconfirmed, Err(ActionError::ConfirmationRequired));
        // As an agent asks: refused without its confirmation.
        assert!(invoke_shell_action(&chrome, "workspace.delete", &serde_json::json!({})).is_err());
        assert_eq!(workspace_names(&chrome), [first.as_str(), "Second"]);

        invoke_shell_action(
            &chrome,
            "workspace.delete",
            &serde_json::json!({ "confirm": true }),
        )
        .expect("a confirmed deletion runs");
        assert_eq!(workspace_names(&chrome), [first.as_str()]);
        assert!(
            matches!(
                invoke_shell_action(
                    &chrome,
                    "workspace.delete",
                    &serde_json::json!({ "confirm": true }),
                ),
                Err(ActionError::Explained(_))
            ),
            "the last workspace stays, and the person is told why"
        );
        assert_eq!(workspace_names(&chrome), [first.as_str()]);
    }

    /// Moving the workspace on screen earlier or later in the list is an
    /// action too, and says so when it is already at that end.
    #[test]
    fn a_workspace_moves_earlier_and_later_in_the_list() {
        let (chrome, second, _hosted) = chrome_with_two_workspaces();
        let first = workspace_names(&chrome)[0].clone();
        invoke_shell_action(&chrome, "workspace.move-earlier", &serde_json::json!({}))
            .expect("the second workspace moves up");
        assert_eq!(workspace_names(&chrome), ["Second", first.as_str()]);
        assert_eq!(
            chrome.borrow().tree().desktop.active,
            second,
            "it stays on screen"
        );
        assert!(
            matches!(
                invoke_shell_action(&chrome, "workspace.move-earlier", &serde_json::json!({})),
                Err(ActionError::Explained(_))
            ),
            "the first workspace moves no further up"
        );
        assert_eq!(workspace_names(&chrome), ["Second", first.as_str()]);
        invoke_shell_action(&chrome, "workspace.move-later", &serde_json::json!({}))
            .expect("and back down");
        assert_eq!(workspace_names(&chrome), [first.as_str(), "Second"]);
        assert!(
            matches!(
                invoke_shell_action(&chrome, "workspace.move-later", &serde_json::json!({})),
                Err(ActionError::Explained(_))
            ),
            "the last workspace moves no further down"
        );
        for id in [
            "workspace.move-earlier",
            "workspace.move-later",
            "group.close",
        ] {
            let contract = shell_action_contract(id);
            assert_eq!(contract.kind, tilo_actions::ActionKind::Undoable, "{id}");
            assert!(contract.can_be_undone(), "{id}");
            assert!(contract.validate_declaration().is_empty(), "{id}");
        }
    }
}
