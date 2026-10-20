//! Feedback about the window in front: the palette's `feedback.open` names
//! that window's app as the report's subject and brings up the one private
//! composer. Nothing about the window is read until the person includes
//! context in a review.

use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::ActionError;
use crate::shell::state::AppKind;
use tilo_desktop::feedback::Focused;

pub(crate) fn open(st: &mut ChromeState) -> Result<(), ActionError> {
    let apps = st.hosted_inputs.in_process().cloned().ok_or_else(|| {
        ActionError::Explained(crate::shell::i18n::t("feedback-action-unavailable"))
    })?;
    // Native tiles other than built-ins carry no app-chosen display name.
    let focused = tilo_desktop::feedback::focused(&st.tree().desktop, |_| None);
    match focused {
        Focused::Composer => {}
        Focused::Nothing => apps.borrow_mut().feedback_about(None),
        Focused::App(subject) => apps.borrow_mut().feedback_about(Some(subject)),
    }
    let open = tilo_desktop::feedback::composer_tab(&st.tree().desktop);
    if let Some((workspace, strip, group, tab)) = open {
        st.focus_tab(workspace, strip, group, tab);
        return Ok(());
    }
    crate::shell::window_actions::open_hosted(
        st,
        AppKind::Other(tilo_desktop::feedback::KIND.into()),
        tilo_desktop::feedback::definition().title(),
        crate::shell::window_actions::Destination::Beside,
    );
    Ok(())
}
