//! Naming the focused tab, for the palette, an agent or a script: the door
//! beside the window menu's Rename tab and the name field in the tab's pill.

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::window_actions::{TabActionError, active_tab_id, rename_tab};

pub(super) const TAB_RENAME_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "The tab's new name; empty gives it back its window's own title. Omit to open the name \
     field in the focused tab for a person to type it. Only a host window's tab (not a Tilo \
     app's, which titles itself) can be named.",
)];

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
