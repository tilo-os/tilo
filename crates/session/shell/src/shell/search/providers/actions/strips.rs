//! Strip actions: create, rename, move and remove the strips of the active
//! workspace, for the palette, an agent or a script.

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

pub(super) const STRIP_CREATE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "1 to 48 characters without control characters. Omit to use the localized default name.",
)];

pub(super) const STRIP_RENAME_PARAMS: &[ActionParam] = &[
    ActionParam::optional(
        "name",
        ParamType::Text,
        "The new name: 1 to 48 characters without control characters. Omit to open the \
         strip switcher's name field on the focused strip for a person to type it.",
    ),
    ActionParam::optional(
        "strip",
        ParamType::Integer,
        "Which strip of the active workspace, counting from 1. Defaults to the focused strip.",
    ),
];

pub(super) const STRIP_MOVE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "strip",
        ParamType::Integer,
        "Which strip of the active workspace to move, counting from 1 at the top.",
    ),
    ActionParam::required(
        "position",
        ParamType::Integer,
        "Where it ends up, counting from 1 at the top; past the last strip means the bottom.",
    ),
];

/// What `strip.remove` may be told: which strip, or the focused one.
pub(super) const STRIP_REMOVE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "strip",
    ParamType::Integer,
    "Which strip of this workspace, counting from 1 at the top. Defaults to the focused strip.",
)];

// A strip name is checked the same wherever it is given: here, and in the
// strip switcher on every host.
pub(crate) use tilo_desktop::strip_switcher::{STRIP_NAME_MAX_CHARS, valid_strip_name};

pub(super) fn create_strip(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let default_name = t("search-action-strip-create-default-name");
    let supplied = args.text("name").unwrap_or(&default_name);
    let name = valid_strip_name(supplied)
        .ok_or_else(|| ActionError::Refused(t("search-action-strip-invalid-name")))?;
    state
        .create_strip(name)
        .map(|_| ())
        .map_err(ActionError::Refused)
}

pub(super) fn rename_strip(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let Some(supplied) = args.text("name") else {
        if args.integer("strip").is_some() {
            return Err(ActionError::Refused(t("search-action-strip-invalid-name")));
        }
        state.request_strip_rename();
        return Ok(());
    };
    let name = valid_strip_name(supplied)
        .ok_or_else(|| ActionError::Refused(t("search-action-strip-invalid-name")))?;
    let strip = match args.integer("strip") {
        None => state.focused_strip_id(),
        Some(index) => usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| {
                let tree = state.tree();
                let id = tree.desktop.active_workspace()?.strips.get(index)?.id;
                drop(tree);
                Some(id)
            }),
    }
    .ok_or_else(|| ActionError::Refused(t("search-action-strip-rename-missing")))?;
    state
        .rename_strip(strip, name)
        .map_err(ActionError::Refused)
}

/// Move a strip of the active workspace to another place in its stack: the
/// overview's Move up and Move down, for an agent or a script.
pub(super) fn move_strip(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let missing = || ActionError::Refused(t("search-action-strip-rename-missing"));
    let index_of = |key: &str| {
        args.integer(key)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| n.checked_sub(1))
    };
    let from = index_of("strip").ok_or_else(missing)?;
    let to = index_of("position").ok_or_else(missing)?;
    let (strip, before) = {
        let tree = state.tree();
        let strips = &tree.desktop.active_workspace().ok_or_else(missing)?.strips;
        let strip = strips.get(from).ok_or_else(missing)?.id;
        let others: Vec<_> = strips
            .iter()
            .map(|s| s.id)
            .filter(|id| *id != strip)
            .collect();
        drop(tree);
        (strip, others.get(to).copied())
    };
    state
        .move_strip(strip, before)
        .map_err(ActionError::Refused)
}

/// Remove an empty strip of the active workspace. A strip that still holds
/// windows is refused rather than closing them, the last strip stays, and a
/// strip another device keeps is theirs to remove, as the strip switcher
/// offers it.
pub(super) fn remove_strip(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let missing = || ActionError::Refused(t("search-action-strip-rename-missing"));
    let strip = match args.integer("strip") {
        None => state.focused_strip_id(),
        Some(index) => usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| {
                let tree = state.tree();
                let id = tree.desktop.active_workspace()?.strips.get(index)?.id;
                drop(tree);
                Some(id)
            }),
    }
    .ok_or_else(missing)?;
    let (empty, last, theirs) = {
        let tree = state.tree();
        let ws = tree.desktop.active_workspace().ok_or_else(missing)?;
        let found = ws
            .strips
            .iter()
            .find(|s| s.id == strip)
            .ok_or_else(missing)?;
        let shape = (
            found.window_groups.is_empty(),
            ws.strips.len() < 2,
            !tree.may_rename_strip(strip),
        );
        drop(tree);
        shape
    };
    if last {
        return Err(ActionError::Refused(t("search-action-strip-remove-last")));
    }
    if theirs {
        return Err(ActionError::Refused(t(
            "search-action-strip-remove-not-yours",
        )));
    }
    if !empty {
        return Err(ActionError::Refused(t(
            "search-action-strip-remove-not-empty",
        )));
    }
    if state.remove_strip_if_empty(strip) {
        Ok(())
    } else {
        Err(missing())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chrome_with_room, invoke_shell_action};

    #[test]
    fn strip_move_puts_a_strip_where_it_is_told_and_refuses_a_strip_that_is_not_there() {
        let chrome = chrome_with_room();
        for name in ["Main", "Second", "Third"] {
            chrome.borrow_mut().create_strip(name).expect("a strip");
        }
        let names = || {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .map(|strip| strip.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let before = names();
        let count = before.len();
        assert!(count >= 3, "{before:?}");
        invoke_shell_action(
            &chrome,
            "strip.move",
            &serde_json::json!({ "strip": count, "position": 1 }),
        )
        .expect("the last strip moves to the top");
        let moved = names();
        assert_eq!(moved.first(), before.last(), "{moved:?}");
        invoke_shell_action(
            &chrome,
            "strip.move",
            &serde_json::json!({ "strip": 1, "position": count + 5 }),
        )
        .expect("past the end means the bottom");
        assert_eq!(names(), before);
        assert!(
            invoke_shell_action(
                &chrome,
                "strip.move",
                &serde_json::json!({ "strip": count + 1, "position": 1 }),
            )
            .is_err(),
            "there is no such strip"
        );
        assert_eq!(names(), before);
    }

    /// What the strip switcher offers to remove, and no more: a strip
    /// another device keeps stays, however empty. A strip of this machine's
    /// goes from the document the person's other devices and the room read
    /// in the same step, as a created or renamed one does, not only from
    /// this screen.
    #[test]
    fn strip_remove_leaves_another_devices_strip_and_reaches_the_document() {
        use crate::shell::state::{Strip, StripId};
        use tilo_desktop_model::ids::compose_id;

        let chrome = chrome_with_room();
        for name in ["Main", "Second"] {
            chrome.borrow_mut().create_strip(name).expect("a strip");
        }
        let other_salt = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.local_salt().wrapping_add(1).max(1)
        };
        let theirs = StripId(compose_id(other_salt, 42));
        chrome
            .borrow()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("a workspace")
            .strips
            .push(Strip::new(theirs, "Theirs"));
        let strips = || {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.strips.iter().map(|s| s.id).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let before = strips();
        let their_place = before
            .iter()
            .position(|id| *id == theirs)
            .expect("their strip is listed")
            + 1;
        assert!(
            invoke_shell_action(
                &chrome,
                "strip.remove",
                &serde_json::json!({ "strip": their_place }),
            )
            .is_err(),
            "another device's strip is theirs to remove"
        );
        assert_eq!(strips(), before, "nothing was removed");

        let heads = chrome.borrow().tree_mut().current_heads();
        invoke_shell_action(&chrome, "strip.remove", &serde_json::json!({ "strip": 2 }))
            .expect("an empty strip of this machine goes");
        assert!(!strips().contains(&before[1]), "{:?}", strips());
        assert_ne!(
            chrome.borrow().tree_mut().current_heads(),
            heads,
            "the removal is in the document the person's devices sync"
        );
    }

    #[test]
    fn strip_remove_drops_an_empty_strip_and_keeps_the_last_one() {
        let chrome = chrome_with_room();
        for name in ["Main", "Second", "Third"] {
            chrome.borrow_mut().create_strip(name).expect("a strip");
        }
        let count = || {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map_or(0, |workspace| workspace.strips.len())
        };
        let before = count();
        assert!(before >= 3);
        invoke_shell_action(
            &chrome,
            "strip.remove",
            &serde_json::json!({ "strip": before }),
        )
        .expect("an empty strip goes");
        assert_eq!(count(), before - 1);
        assert!(
            invoke_shell_action(
                &chrome,
                "strip.remove",
                &serde_json::json!({ "strip": before })
            )
            .is_err(),
            "there is no such strip"
        );
        while count() > 1 {
            invoke_shell_action(&chrome, "strip.remove", &serde_json::json!({ "strip": 1 }))
                .expect("empty strips go");
        }
        assert!(
            invoke_shell_action(&chrome, "strip.remove", &serde_json::json!({ "strip": 1 }))
                .is_err(),
            "a workspace keeps its last strip"
        );
        assert_eq!(count(), 1);
    }
}
