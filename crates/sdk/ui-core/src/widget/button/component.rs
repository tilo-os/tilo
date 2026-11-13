//! [`Button`] as a described component.

use crate::component::{Component, Described, Event};
use crate::icons::Icon;
use crate::widget::{ButtonRole, ControlSize};

use super::Button;
use crate::widget::{BadgeContent, ChipStyle, Tone};

/// What stands on the icon: the count, else the dot, else nothing.
fn count_content(count: Option<i64>, dot: bool) -> Option<BadgeContent> {
    match count {
        Some(n) if n > 0 => Some(BadgeContent::Count(u32::try_from(n).unwrap_or(u32::MAX))),
        _ if dot => Some(BadgeContent::Dot),
        _ => None,
    }
}

/// A button: pressing it does one thing. Its role says what kind of thing, which decides its
/// whole look: the window's one primary action, a destructive one, or an ordinary one.
#[derive(Clone, Component)]
#[component(name = "Button", role = "Button", label = "label")]
pub struct ButtonProps {
    /// What pressing it does, in a word or two. An icon-only button is still named by it.
    #[component(required, positional)]
    pub label: String,
    /// How assistive technology names it, where that says more than the label shows: the
    /// charge reads "61%" beside its charging glyph and is named "61%, charging". Unset, the
    /// label names it.
    pub accessible_name: Option<String>,
    /// A glyph before the label, or instead of it when `icon_only`.
    pub icon: Option<Icon>,
    /// Show only the icon, the label naming the button to assistive technology.
    pub icon_only: bool,
    /// Who is here, as their faces: up to three faces, then a face counting the rest, and
    /// nothing else on the button at any width. `icon_only`'s rule holds for it too: the
    /// label is the name assistive technology hears, naming the people ("Mia Clayton and Sam
    /// Okafor are here"). One press opens the roster of everyone here.
    pub faces: Vec<String>,
    /// What kind of action it is: `secondary` unless said, `plain` for an icon-only one.
    pub role: Option<ButtonRole>,
    /// A toggle button, and whether it is on: on, it stands on a lift of its ground and is
    /// announced as pressed.
    pub toggled: Option<bool>,
    /// How prominent it is: `regular` unless said.
    pub size: ControlSize,
    /// Whether it can be pressed now. A button with nothing to do never can.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the label does not say: the requirement an action
    /// does not meet. Its tooltip, and the description assistive technology reads beside the
    /// name it announces.
    pub description: Option<String>,
    /// The plain key that presses it while its window has the keyboard (and, on a row's action,
    /// while that row is selected), as the app declared it (`key: "e"`): the host answers it
    /// beside the pressed control's own event. Never drawn here; the header shows it at the
    /// action's menu row.
    #[component(data)]
    pub key: Option<String>,
    /// How many new things wait behind it (unread notifications, new mail), on an icon-only
    /// button: a count standing on its icon's top-trailing corner, as an app icon's does, and
    /// nothing at 0. The button keeps room for it, so it never covers the control beside it.
    /// The label says the count to assistive technology.
    pub count: Option<i64>,
    /// Something new behind it without a number: a dot on its icon's corner.
    pub dot: bool,
    /// It needs the person: the count or the dot in the warning tone, not in your colour.
    pub alert: bool,
    /// It belongs in its header's menu rather than in the row
    /// ([`crate::widget::Button::in_menu`]).
    pub in_menu: bool,
    /// The run of its owner's actions it stands in (`group: "Due"`), as the app names it: the
    /// header's menu shows rows of one run together, under the run's own row.
    pub group: Option<String>,
    /// It was pressed (a click, Enter or Space, an assistive technology).
    #[component(event(press))]
    pub press: Event,
}

impl Default for ButtonProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            accessible_name: None,
            icon: None,
            icon_only: false,
            faces: Vec::new(),
            role: None,
            toggled: None,
            size: ControlSize::default(),
            enabled: true,
            description: None,
            key: None,
            count: None,
            dot: false,
            alert: false,
            in_menu: false,
            group: None,
            press: Event::None,
        }
    }
}

impl Described for Button {
    type Props = ButtonProps;

    fn from_props(key: &str, props: ButtonProps) -> Self {
        let ButtonProps {
            label,
            accessible_name,
            icon,
            icon_only,
            faces,
            role,
            toggled,
            size,
            enabled,
            description,
            key: _,
            count,
            dot,
            alert,
            in_menu,
            group,
            press,
        } = props;
        let enabled = enabled && !press.is_none();
        let mut button = match icon {
            _ if !faces.is_empty() => Self::new(key, "")
                .with_accessible_name(&label)
                .tooltip(label.clone())
                .with_faces(faces),
            Some(icon) if icon_only => Self::icon_only(key, icon, &label),
            Some(icon) => Self::new(key, &label).with_icon(icon),
            None => Self::new(key, &label),
        };
        if let Some(name) = accessible_name {
            button = button.with_accessible_name(&name);
        }
        if let Some(role) = role {
            button = button.role(role);
        }
        if let Some(on) = toggled {
            button = button.toggled(on);
        }
        if size != ControlSize::default() {
            button = button.control_size(size);
        }
        if let Some(content) = count_content(count, dot) {
            let style = if alert {
                ChipStyle::Tinted(Tone::Warning)
            } else {
                ChipStyle::Live
            };
            button = button.with_count(content, style);
        }
        let button = if in_menu { button.in_menu() } else { button };
        let mut button = button.with_enabled(enabled);
        if let Some(why) = description.filter(|why| !why.trim().is_empty()) {
            button = button.with_description(&why).tooltip(why);
        }
        if let Some(group) = group {
            button = button.with_menu_group(group);
        }
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
        let faces = !props.faces.is_empty();
        let role = props.role.unwrap_or(if props.icon_only || faces {
            ButtonRole::Plain
        } else {
            ButtonRole::default()
        });
        // The name assistive technology hears: what the app stated, else the label an
        // icon-only or faces button shows to no one, else none.
        let heard = props
            .accessible_name
            .as_deref()
            .or_else(|| (props.icon_only || faces).then_some(props.label.as_str()));
        let named = if props.icon_only || faces {
            self.label.is_empty() && heard == self.accessible_name.as_deref()
        } else {
            props.label == self.label && heard == self.accessible_name.as_deref()
        };
        let same_face = named
            && props.faces.len() == self.faces.len()
            && props
                .faces
                .iter()
                .zip(&self.faces)
                .all(|(name, (shown, _))| name == shown)
            && props.icon == self.icon
            && role == self.role
            && size == self.control_size
            && props.toggled.is_some() == self.toggled.is_some()
            && self.shows_count(count_content(props.count, props.dot).as_ref(), props.alert)
            && props.in_menu == self.is_menu_only()
            && props.group == self.menu_group
            && props.description == self.description
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
