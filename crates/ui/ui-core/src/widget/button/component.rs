//! [`Button`] as a described component.

use crate::widget::{ButtonRole, ControlSize};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::Person,
};

use super::Button;
use crate::{widget::BadgeContent, widget::ChipStyle};
use tilo_ui_base::widget::Tone;

fn count_content(count: Option<i64>, dot: bool) -> Option<BadgeContent> {
    match count {
        Some(n) if n > 0 => Some(BadgeContent::Count(u32::try_from(n).unwrap_or(u32::MAX))),
        _ if dot => Some(BadgeContent::Dot),
        _ => None,
    }
}

/// A button: pressing it does one thing.
#[derive(Clone, Component)]
#[component(name = "Button", role = "Button", label = "label")]
pub struct ButtonProps {
    /// What pressing it does, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// How assistive technology names it, where that says more than the label shows: the charge reads "61%"
    /// beside its charging glyph and is named "61%, charging".
    pub accessible_name: Option<String>,
    /// A glyph before the label, or instead of it when `icon_only`.
    pub icon: Option<Icon>,
    /// Show only the icon, the label naming the button to assistive technology.
    pub icon_only: bool,
    /// Who is here, as their faces: up to three faces, then a face counting the rest, and nothing else on
    /// the button at any width. Each is the member, so their face wears the picture they set.
    pub faces: Vec<Person>,
    /// What kind of action it is: `secondary` unless said, `plain` for an icon-only one.
    pub role: Option<ButtonRole>,
    /// A toggle button, and whether it is on: on, it stands on a lift of its ground and is announced as
    /// pressed.
    pub toggled: Option<bool>,
    /// How prominent it is: `regular` unless said.
    pub size: ControlSize,
    /// Whether it can be pressed now.
    pub enabled: bool,
    /// Whether it is the answer the surface it stands in starts on: an ask that opens puts the
    /// keyboard here, so Enter gives this answer. A view marks the answer its surface opens on,
    /// and leaves it off for every other control beside it.
    pub default_focus: bool,
    /// Why it cannot be pressed now, where the label does not say: the requirement an action does not meet.
    pub description: Option<String>,
    /// What it says is news: a screen reader announces it when it appears or changes, without the
    /// person moving focus — `polite` waits for a pause, `assertive` speaks at once.
    pub live: Option<tilo_ui_base::a11y::Politeness>,
    /// The plain key that presses it while its window has the keyboard (and, on a row's action, while that
    /// row is selected), as the app declared it (`key: "e"`): the host answers it beside the pressed
    /// control's own event. A control of the view's own press shows it beside the label as the keyboard
    /// in use spells it; a button for an action keeps its face, its key shown where the action is listed.
    #[component(data)]
    pub key: Option<String>,
    /// The plain keys that press it while its window has the keyboard, each as the app declared
    /// it (`keys: ["Right", "Space"]`): every one answers the same press. `key` is the one its
    /// keys show; a control that names several usually names none.
    #[component(data)]
    pub keys: Vec<String>,
    /// How many new things wait behind it (unread notifications, new mail), on an icon-only button: a count
    /// on its icon's top-trailing corner, as an app icon's does, and nothing at 0.
    pub count: Option<i64>,
    /// Something new behind it without a number: a dot on its icon's corner.
    pub dot: bool,
    /// It needs the person: the count or the dot in the warning tone, not in your colour.
    pub alert: bool,
    /// It belongs in its header's menu rather than in the row ([`crate::widget::Button::in_menu`]).
    pub in_menu: bool,
    /// The run of its owner's actions it stands in (`group: "Due"`), as the app names it: the header's menu
    /// shows rows of one run together, under the run's own row.
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
            default_focus: false,
            description: None,
            live: None,
            key: None,
            keys: Vec::new(),
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
            default_focus,
            description,
            live,
            key: shortcut_key,
            keys: _,
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
                .with_faces_of(faces),
            // An icon alone is named by its accessible name when the label is empty
            // (`ui.Button("", icon: .refresh, icon_only: true, accessible_name: ...)`).
            Some(icon) if icon_only => {
                let name = match accessible_name.as_deref() {
                    Some(name) if label.is_empty() => name,
                    _ => &label,
                };
                Self::icon_only(key, icon, name)
            }
            Some(icon) => Self::new(key, &label).with_icon(icon),
            None => Self::new(key, &label),
        };
        if let Some(name) = accessible_name {
            button = button.with_accessible_name(&name);
        }
        if let Some(politeness) = live {
            button = button.with_live(politeness);
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
        let button = if default_focus {
            button.with_default_focus()
        } else {
            button
        };
        let mut button = button.with_enabled(enabled);
        if let Some(why) = description.filter(|why| !why.trim().is_empty()) {
            button = button.with_description(&why).tooltip(why);
        }
        if let Some(group) = group {
            button = button.with_menu_group(group);
        }
        // The key the view declared answers its press and shows beside the label, as a menu row's
        // does, so the shortcut stays discoverable where no menu lists it. A button for an action
        // keeps its face: the action's key is shown where the action is listed, and the bar's
        // controls stay as the app laid them out.
        if let Some(key) = shortcut_key.filter(|key| !key.trim().is_empty())
            && !matches!(&press, Event::Action(_))
        {
            button = button.shortcut_hint(tilo_ui_base::platform::Shortcut::from(key.as_str()));
        }
        match press {
            Event::None => button,
            Event::Action(binding) => button.on_action(binding),
            Event::View(intent) => button.on_open(intent.sender()),
        }
    }

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
        // The shortcut the view declared shows beside the label, so a key the app or the person
        // moved rebuilds the face that names it. A key computed to nothing is no hint.
        let shortcut = props
            .key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .map(tilo_ui_base::platform::Shortcut::from);
        let same_face = named
            && props.faces.len() == self.faces.len()
            && props
                .faces
                .iter()
                .zip(&self.faces)
                .all(|(name, shown)| name == shown)
            && props.icon == self.icon
            && role == self.role
            && size == self.control_size
            && props.toggled.is_some() == self.toggled.is_some()
            && shortcut == self.declared_key()
            && self.shows_count(count_content(props.count, props.dot).as_ref(), props.alert)
            && props.in_menu == self.is_menu_only()
            && props.group == self.menu_group
            && props.description == self.description
            && props.live == self.live
            && props.press.is_none() == (self.action.is_none() && self.on_click.is_none());
        if !same_face || matches!(props.press, Event::Action(_)) {
            return false;
        }
        if let Some(on) = props.toggled {
            self.set_toggled(on);
        }
        self.default_focus = props.default_focus;
        self.set_enabled(props.enabled && !props.press.is_none());
        self.live = props.live;
        true
    }
}
