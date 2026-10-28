//! [`EmptyState`] as a described component.

use crate::component::{Component, Described, Event};
use crate::icons::Icon;

use super::EmptyState;

/// What a place says while it holds nothing: one sentence, and at most one action that starts
/// filling it.
#[derive(Component, Default)]
#[component(name = "EmptyState", role = "Group", label = "title")]
pub struct EmptyStateProps {
    /// Why it is empty, in one short sentence: "No notes yet".
    #[component(required, positional)]
    pub title: String,
    /// One more sentence under it, when the first does not say enough.
    pub message: Option<String>,
    /// A large glyph above the title: the app's, or the kind of thing that would be here.
    pub icon: Option<Icon>,
    /// The label of the one action that fills it: "New note".
    pub action: Option<String>,
    /// Sit within the content, as a list without rows, rather than fill the window.
    pub inline: bool,
    /// The action was pressed.
    #[component(event)]
    pub act: Event,
}

impl Described for EmptyState {
    type Props = EmptyStateProps;

    fn from_props(key: &str, props: EmptyStateProps) -> Self {
        let EmptyStateProps {
            title,
            message,
            icon,
            action,
            inline,
            act,
        } = props;
        let mut state = Self::new(title).with_id(key);
        if let Some(message) = message {
            state = state.with_message(message);
        }
        if let Some(icon) = icon {
            state = state.with_icon(icon);
        }
        if let Some(label) = action {
            state = state.with_action(label).with_action_id(key);
            state = match act {
                Event::None => state,
                Event::Action(binding) => state.on_action(binding),
                Event::View(intent) => state.on_view(intent.sender()),
            };
        }
        if inline { state.inline() } else { state }
    }
}
