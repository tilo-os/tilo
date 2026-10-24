//! Window actions the overlay host carries out: the focused window group
//! moved into a strip chosen by name, and the outcome every rearrangement
//! reports once it ran, so a move past the edge says nothing changed.

use std::rc::Rc;

use settings_store::hardware::ShortcutAction;

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

/// [`super::request_window_shortcut`] for an operation on the focused window
/// group, refused at once when no window has focus.
pub(super) fn request_group_shortcut(
    st: &mut ChromeState,
    action: ShortcutAction,
) -> Result<(), ActionError> {
    if crate::shell::window_actions::active_group_id(st).is_none() {
        return Err(no_window());
    }
    st.request_shortcut_action(action);
    Ok(())
}

pub(super) fn no_window() -> ActionError {
    ActionError::Explained(t("search-action-window-none"))
}

fn window_unchanged() -> ActionError {
    ActionError::Explained(t("search-action-window-unchanged"))
}

/// How the window operations an invocation queued (`outcomes`) went: done
/// when nothing was queued; else answered once the overlay host ran them,
/// a success when any changed something and "nothing changed" otherwise,
/// so a move past the edge does not report a move.
pub(super) fn window_outcome(
    outcomes: Vec<crate::shell::chrome_state::WindowOutcome>,
) -> crate::shell::search::InvocationDisposition {
    use crate::shell::search::{ActionReceipt, InvocationDisposition};
    if outcomes.is_empty() {
        return InvocationDisposition::Accepted(ActionReceipt::completed());
    }
    InvocationDisposition::Pending(Rc::new(move || {
        let answered: Option<Vec<bool>> = outcomes.iter().map(|outcome| outcome.get()).collect();
        match answered {
            None => Ok(None),
            Some(changed) if changed.contains(&true) => Ok(Some(ActionReceipt::completed())),
            Some(_) => Err(window_unchanged()),
        }
    }))
}

/// Carrying the focused window group into a strip the caller names, as the
/// window menu's Move to strip does.
pub(super) const GROUP_MOVE_TO_STRIP_ID: &str = "group.move-to-strip";

pub(super) const GROUP_MOVE_TO_STRIP_PARAMS: &[ActionParam] = &[ActionParam::required(
    "strip",
    ParamType::Integer,
    "Which strip of the active workspace, counting from 1 at the top.",
)];

/// [`GROUP_MOVE_TO_STRIP_PARAMS`], each strip of the active workspace
/// offered by the name the strip switcher shows.
pub(super) fn group_move_to_strip_params(state: &ChromeState) -> Vec<ActionParam> {
    let strips: Vec<(String, String)> = {
        let tree = state.tree();
        let strips = tree
            .desktop
            .active_workspace()
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .enumerate()
                    .map(|(index, strip)| {
                        (
                            index.saturating_add(1).to_string(),
                            crate::shell::window_menu::strip_shown_name(&strip.name, index),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        drop(tree);
        strips
    };
    GROUP_MOVE_TO_STRIP_PARAMS
        .iter()
        .cloned()
        .map(|param| param.with_labelled_choices(strips.clone()))
        .collect()
}

/// Queue the focused window group's move into the `strip`th strip of the
/// active workspace for the overlay host. A group already there changes
/// nothing, and says so.
pub(super) fn move_group_to_strip(
    st: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let group = crate::shell::window_actions::active_group_id(st).ok_or_else(no_window)?;
    let missing = || ActionError::Explained(t("search-action-strip-rename-missing"));
    let index = args
        .integer("strip")
        .and_then(|number| usize::try_from(number).ok())
        .and_then(|number| number.checked_sub(1))
        .ok_or_else(missing)?;
    let (strip, already_there) = {
        let tree = st.tree();
        let strip = tree
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.strips.get(index))
            .map(|strip| {
                (
                    strip.id,
                    strip
                        .window_groups
                        .iter()
                        .any(|candidate| candidate.id == group),
                )
            });
        drop(tree);
        strip.ok_or_else(missing)?
    };
    if already_there {
        return Err(window_unchanged());
    }
    st.request_window(crate::shell::chrome_state::WindowRequest::MoveGroupToStrip { group, strip });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chrome_with_room, invoke_shell_action};
    use crate::shell::search::action_api::ActionError;

    /// A window action names what is wrong when no window has focus, rather
    /// than queueing a move for nothing.
    #[test]
    fn window_actions_without_a_focused_window_say_so() {
        let chrome = chrome_with_room();
        for (id, args) in [
            ("group.move-left", serde_json::json!({})),
            ("group.maximize", serde_json::json!({})),
            ("group.close", serde_json::json!({})),
            ("group.move-to-strip", serde_json::json!({ "strip": 1 })),
        ] {
            assert!(
                matches!(
                    invoke_shell_action(&chrome, id, &args),
                    Err(ActionError::Explained(_))
                ),
                "{id}"
            );
        }
        assert!(chrome.borrow_mut().take_window_requests().is_empty());
    }
}
