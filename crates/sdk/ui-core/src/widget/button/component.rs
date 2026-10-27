//! [`Button`] as a described component.

use crate::component::{Component, Described, Event};
use crate::icons::Icon;
use crate::widget::{ButtonRole, ControlSize};

use super::Button;

/// A button: pressing it does one thing. Its role says what kind of thing, which decides its
/// whole look: the window's one primary action, a destructive one, or an ordinary one.
#[derive(Component)]
#[component(name = "Button", role = "Button", label = "label")]
pub struct ButtonProps {
    /// What pressing it does, in a word or two. An icon-only button is still named by it.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, or instead of it when `icon_only`.
    pub icon: Option<Icon>,
    /// Show only the icon, the label naming the button to assistive technology.
    pub icon_only: bool,
    /// What kind of action it is: `secondary` unless said.
    pub role: ButtonRole,
    /// How prominent it is: `regular` unless said.
    pub size: ControlSize,
    /// Whether it can be pressed now. A button with nothing to do never can.
    pub enabled: bool,
    /// It was pressed (a click, Enter or Space, an assistive technology).
    #[component(event)]
    pub press: Event,
}

impl Default for ButtonProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            icon: None,
            icon_only: false,
            role: ButtonRole::default(),
            size: ControlSize::default(),
            enabled: true,
            press: Event::None,
        }
    }
}

impl Described for Button {
    type Props = ButtonProps;

    fn from_props(key: &str, props: ButtonProps) -> Self {
        let ButtonProps {
            label,
            icon,
            icon_only,
            role,
            size,
            enabled,
            press,
        } = props;
        let enabled = enabled && !press.is_none();
        let button = match icon {
            Some(icon) if icon_only => Self::icon_only(key, icon, &label),
            Some(icon) => Self::new(key, &label).with_icon(icon),
            None => Self::new(key, &label),
        };
        let button = button.role(role).control_size(size).with_enabled(enabled);
        match press {
            Event::None => button,
            Event::Action(binding) => button.on_action(binding),
            Event::View(intent) => button.on_view(intent.sender()),
        }
    }

    /// Whether it can be pressed changes in place; anything else builds it again. Its press
    /// acts through its key, so the same event on the same key is the same press.
    fn set_props(&mut self, props: ButtonProps) -> bool {
        let size = props.size;
        let same_face = props.label == self.label
            && props.icon == self.icon
            && props.role == self.role
            && size == self.control_size
            && props.press.is_none() == (self.action.is_none() && self.on_click.is_none());
        if !same_face || matches!(props.press, Event::Action(_)) {
            return false;
        }
        self.set_enabled(props.enabled && !props.press.is_none());
        true
    }
}
