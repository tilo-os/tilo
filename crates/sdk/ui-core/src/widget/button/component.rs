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
    /// What kind of action it is: `secondary` unless said, `plain` for an icon-only one.
    pub role: Option<ButtonRole>,
    /// A toggle button, and whether it is on: on, it stands on a lift of its ground and is
    /// announced as pressed.
    pub toggled: Option<bool>,
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
            role: None,
            toggled: None,
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
            toggled,
            size,
            enabled,
            press,
        } = props;
        let enabled = enabled && !press.is_none();
        let mut button = match icon {
            Some(icon) if icon_only => Self::icon_only(key, icon, &label),
            Some(icon) => Self::new(key, &label).with_icon(icon),
            None => Self::new(key, &label),
        };
        if let Some(role) = role {
            button = button.role(role);
        }
        if let Some(on) = toggled {
            button = button.toggled(on);
        }
        if size != ControlSize::default() {
            button = button.control_size(size);
        }
        let button = button.with_enabled(enabled);
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
        let role = props.role.unwrap_or(if props.icon_only {
            ButtonRole::Plain
        } else {
            ButtonRole::default()
        });
        let named = if props.icon_only {
            self.label.is_empty() && self.accessible_name.as_deref() == Some(props.label.as_str())
        } else {
            props.label == self.label
        };
        let same_face = named
            && props.icon == self.icon
            && role == self.role
            && size == self.control_size
            && props.toggled.is_some() == self.toggled.is_some()
            && props.press.is_none() == (self.action.is_none() && self.on_click.is_none());
        if !same_face || matches!(props.press, Event::Action(_)) {
            return false;
        }
        if let Some(on) = props.toggled {
            self.set_toggled(on);
        }
        self.set_enabled(props.enabled && !props.press.is_none());
        true
    }
}
