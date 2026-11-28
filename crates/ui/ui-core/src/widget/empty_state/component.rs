//! [`EmptyState`] as a described component.

use tilo_ui_base::icons::Icon;
use tilo_ui_base::{component::Component, component::Described, component::Event};

use super::EmptyState;

/// What a place says while it holds nothing: a sentence and at most one action.
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
    /// One quieter line under the action, when its label alone does not say enough: "Add the
    /// people you message in Contacts". With `hint_act` it is a route of its own to press, not
    /// only a sentence.
    pub hint: Option<String>,
    /// Sit within the content, as a list without rows, rather than fill the window.
    pub inline: bool,
    /// Something failed: the glyph takes the danger colour and the message reads as an error.
    pub error: bool,
    /// The action was pressed.
    #[component(event(press))]
    pub act: Event,
    /// The hint was pressed: the same way out, said in a sentence.
    #[component(event(press))]
    pub hint_act: Event,
}

impl Described for EmptyState {
    type Props = EmptyStateProps;

    fn from_props(key: &str, props: EmptyStateProps) -> Self {
        let EmptyStateProps {
            title,
            message,
            icon,
            action,
            hint,
            inline,
            error,
            act,
            hint_act,
        } = props;
        let mut state = Self::new(title).with_id(key);
        if let Some(message) = message {
            state = state.with_message(message);
        }
        if let Some(icon) = icon {
            state = state.with_icon(icon);
        }
        state.error = error;
        if let Some(label) = action {
            state = state.with_action(label).with_action_id(key);
            state = match act {
                Event::None => state,
                Event::Action(binding) => state.on_action(binding),
                Event::View(intent) => state.on_open(intent.sender()),
            };
        }
        if let Some(hint) = hint {
            state = state.with_hint(hint);
            state = match hint_act {
                Event::None => state,
                Event::Action(binding) => state.on_hint_action(binding),
                Event::View(intent) => state.on_hint_open(intent.sender()),
            };
        }
        if inline { state.inline() } else { state }
    }

    fn set_props(&mut self, props: EmptyStateProps) -> bool {
        let EmptyStateProps {
            title,
            message,
            icon,
            action,
            hint,
            inline,
            error,
            act,
            hint_act,
        } = props;
        self.title = title;
        self.message = message;
        self.icon = icon;
        self.inline = inline;
        self.error = error;
        self.action_id = action.as_ref().map(|_| self.id.clone().unwrap_or_default());
        self.action_label = action;
        self.handler = match act {
            Event::None => None,
            Event::Action(binding) => Some(super::Handler::Binding(binding)),
            Event::View(intent) => Some(super::Handler::Press(std::rc::Rc::new(
                std::cell::RefCell::new(Box::new(intent.sender())),
            ))),
        };
        self.hint = hint;
        self.hint_handler = match hint_act {
            Event::None => None,
            Event::Action(binding) => Some(super::Handler::Binding(binding)),
            Event::View(intent) => Some(super::Handler::Press(std::rc::Rc::new(
                std::cell::RefCell::new(Box::new(intent.sender())),
            ))),
        };
        self.resolve();
        self.rebuild_engine();
        self.layout_dirty = true;
        self.drawn = None;
        true
    }
}
