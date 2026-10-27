//! Naming the focused tab and taking it to another workspace, for the
//! palette, an agent or a script: the doors beside the window menu's Rename
//! tab and Move to workspace, and the name field in the tab's pill.

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::window_actions::{
    TabActionError, active_tab_id, move_tab_to_workspace, rename_tab,
};

pub(super) const TAB_RENAME_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "The tab's new name; empty gives it back its window's own title. Omit to open the name \
     field in the focused tab for a person to type it. Only a host window's tab (not a Tilo \
     app's, which titles itself) can be named.",
)];

/// What `tab.move-to-workspace` is told: where the focused tab goes.
pub(super) const TAB_MOVE_TO_WORKSPACE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "workspace",
    ParamType::Integer,
    "Which of your workspaces, counting from 1 in the order the workspace menu lists them. \
     A window an app draws itself asks the person what goes with it before it moves.",
)];

/// Take the focused tab to another workspace, as the window menu's Move to
/// workspace does: the tab the shell draws moves at once, an app's window
/// once the person agrees on the sheet that asks what goes with it.
pub(super) fn move_focused_tab_to_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let refuse = |state: &mut ChromeState, message: String| {
        state.push_warning_notification(t("host-tab-move-failed"), &message);
        ActionError::Refused(message)
    };
    let Some(tab) = active_tab_id(state) else {
        let message = TabActionError::NoActiveTab.user_message();
        return Err(refuse(state, message));
    };
    let target = args
        .integer("workspace")
        .and_then(|index| usize::try_from(index).ok()?.checked_sub(1))
        .and_then(|index| {
            let tree = state.tree();
            let id = tree
                .desktop
                .workspaces
                .get(index)
                .map(|workspace| workspace.id);
            drop(tree);
            id
        });
    let Some(target) = target else {
        return Err(ActionError::Refused(t(
            "search-action-tab-move-to-workspace-missing",
        )));
    };
    move_tab_to_workspace(state, tab, target)
        .map(drop)
        .map_err(|err| refuse(state, err.user_message()))
}

/// Name the focused host window's tab, or open its name field when no name
/// is given.
pub(super) fn rename_focused_tab(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let refuse = |state: &mut ChromeState, err: TabActionError| {
        let message = err.user_message();
        state.push_warning_notification(t("search-action-tab-rename-failed"), &message);
        ActionError::Refused(message)
    };
    let Some(tab) = active_tab_id(state) else {
        return Err(refuse(state, TabActionError::NoActiveTab));
    };
    if let Some(name) = args.text("name") {
        return rename_tab(state, tab, name)
            .map(|_| ())
            .map_err(|err| refuse(state, err));
    }
    if !crate::shell::tab_binding::may_rename_tab(&state.tree(), tab) {
        return Err(refuse(state, TabActionError::NotRenamable));
    }
    state.tab_rename_request = Some(tab);
    state.bump();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chrome_with_room, invoke_shell_action};
    use crate::shell::chrome_state::Chrome;
    use crate::shell::state::{AppKind, TabId, WorkspaceId};
    use crate::shell::window_actions::{Destination, open_hosted};

    /// The workspace whose strips hold `tab`.
    fn workspace_of(chrome: &Chrome, tab: TabId) -> Option<WorkspaceId> {
        let state = chrome.borrow();
        let tree = state.tree();
        let found = tree.desktop.workspaces.iter().find_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .any(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
                .then_some(workspace.id)
        });
        drop(tree);
        found
    }

    #[test]
    fn the_focused_tab_moves_to_the_workspace_it_is_told_and_nowhere_else() {
        let chrome = chrome_with_room();
        let first = chrome.borrow().tree().desktop.active;
        chrome.borrow_mut().create_strip("Main").expect("a strip");
        let notes = open_hosted(
            &mut chrome.borrow_mut(),
            AppKind::Other("Notes".into()),
            "Notes",
            Destination::Beside,
        );
        let second = chrome.borrow_mut().add_workspace("Second");
        assert!(chrome.borrow_mut().switch_to_workspace(first));
        assert_eq!(workspace_of(&chrome, notes), Some(first));

        for (workspace, why) in [
            (9, "there is no ninth workspace"),
            (0, "counting starts at 1"),
        ] {
            assert!(
                invoke_shell_action(
                    &chrome,
                    "tab.move-to-workspace",
                    &serde_json::json!({ "workspace": workspace }),
                )
                .is_err(),
                "{why}"
            );
        }
        assert!(
            invoke_shell_action(
                &chrome,
                "tab.move-to-workspace",
                &serde_json::json!({ "workspace": 1 }),
            )
            .is_err(),
            "the tab is in that workspace already"
        );
        assert_eq!(workspace_of(&chrome, notes), Some(first));

        invoke_shell_action(
            &chrome,
            "tab.move-to-workspace",
            &serde_json::json!({ "workspace": 2 }),
        )
        .expect("the focused tab moves to the second workspace");
        assert_eq!(workspace_of(&chrome, notes), Some(second));
    }
}
