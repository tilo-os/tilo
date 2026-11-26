//! Where an agent works, for the shell to go there, and the actions it narrates, drawn as its
//! cursor where it acted.

use tilo_actions::SemanticInteraction;

use super::Person;

/// Go to where `who` (a roster id among `people`) works, once: the window of theirs shown here,
/// else a window of the app they are in. Nothing moves when neither stands here. Every shell (a
/// device's session, a page) goes there through this.
pub fn go_to(handle: &crate::desktop::state::DesktopHandle, people: &[Person], who: &str) {
    let Some(person) = people.iter().find(|person| person.id == who) else {
        return;
    };
    if let Some(tab) = person.tab {
        handle.focus_tab(tab);
    } else if let Some(app) = person.followed_app() {
        handle.focus_app(app);
    }
}

/// `narrated` drawn into this shell's `presence`: each action moves its agent's cursor to the
/// control it is bound to, over the window that shows its record. Whether it drew any.
pub fn apply_narrated(
    presence: &mut tilo_ui_base::presence::PresenceCtx,
    narrated: &[SemanticInteraction],
) -> bool {
    let mut shown = false;
    for interaction in narrated {
        shown |= presence
            .apply_interaction(interaction.clone())
            .unwrap_or(false);
    }
    shown
}
