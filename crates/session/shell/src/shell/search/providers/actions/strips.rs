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
/// windows is refused rather than closing them, and the last strip stays.
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
    let (empty, last) = {
        let tree = state.tree();
        let ws = tree.desktop.active_workspace().ok_or_else(missing)?;
        let found = ws
            .strips
            .iter()
            .find(|s| s.id == strip)
            .ok_or_else(missing)?;
        let shape = (found.window_groups.is_empty(), ws.strips.len() < 2);
        drop(tree);
        shape
    };
    if last {
        return Err(ActionError::Refused(t("search-action-strip-remove-last")));
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

    /// The strip an agent is given is made as every strip is, through the
    /// room's document, so a window moved onto it lands there instead of
    /// being refused; it takes no focus of its own.
    #[test]
    fn an_agents_new_strip_is_in_the_document_and_takes_a_moved_window() {
        use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup, WindowWidth};
        let chrome = chrome_with_room();
        let main = chrome.borrow_mut().create_strip("Main").expect("a strip");
        let group = 7001;
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
            workspace.strips[0].window_groups.push(WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: 7002,
                    title: "Notes".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Test".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                }],
                active_tab: 7002,
                width: WindowWidth::Fraction(500),
                maximized: false,
                focus_history: Vec::new(),
            });
            tree.seed_doc_from_cache();
        }
        let focused = chrome.borrow().focused_strip_id();
        assert_eq!(focused, Some(main));
        let agent = chrome
            .borrow_mut()
            .agent_strip(None, |_| true, "Agent a")
            .expect("a strip for the agent");
        assert_ne!(agent, main);
        assert_eq!(
            chrome.borrow().focused_strip_id(),
            focused,
            "focus stays where it was"
        );
        let moved = chrome.borrow().tree_mut().commit_command(
            &tilo_desktop_model::commands::DesktopCommand::MoveGroup {
                group,
                strip: agent.raw(),
                before: None,
            },
        );
        assert_eq!(moved, Ok(true), "the window lands on the agent's strip");
        let landed = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.strips.iter().find(|strip| strip.id == agent))
            .map(|strip| strip.window_groups.iter().map(|g| g.id).collect::<Vec<_>>());
        assert_eq!(landed, Some(vec![group]));
    }
}
