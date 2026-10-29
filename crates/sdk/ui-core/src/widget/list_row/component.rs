//! [`ListRow`] as a described component.

use crate::component::{Bound, Component, Described, Event, ResourceId, Value};
use crate::icons::Icon;
use crate::widget::{AvatarProps, Element};

use super::{Leading, ListRow};

/// A row of a list: what an item is, in one line, with the start of its content under it and a
/// quiet fact on its trailing edge. Pressing it activates it: a list opens what it shows.
#[derive(Component, Default)]
#[component(
    name = "ListRow",
    role = "ListItem",
    label = "title",
    presents = "item"
)]
pub struct ListRowProps {
    /// The row's stable key: selection, focus and motion follow it.
    #[component(key)]
    pub key: String,
    /// What the item is: its title. A row that names none shows its item's.
    #[component(positional)]
    pub title: String,
    /// The resource the row shows, which activating the row opens.
    pub item: Option<ResourceId>,
    /// The line under the title: the start of the item's content, the last message.
    pub secondary: Option<String>,
    /// A quiet fact on the trailing edge: when it happened, how many.
    pub meta: Option<String>,
    /// The item's value on the trailing edge, as a setting shows it.
    pub value: Option<String>,
    /// A short code in the leading column: a method, a kind.
    pub badge: Option<String>,
    /// A mark after the title: pinned, starred.
    pub mark: Option<Icon>,
    /// A glyph in the leading column: what kind of item it is.
    pub icon: Option<Icon>,
    /// Whether the row is selected in its list.
    pub selected: bool,
    /// Keep the room an avatar takes without showing one, so the row's text lines up with the
    /// rows of its list that show one.
    pub avatar_space: bool,
    /// The avatar in the leading column: who the item is, or who last wrote in it.
    #[component(slot)]
    pub avatar: Option<AvatarProps>,
    /// A control on the trailing edge, which moves under the text when the row is narrow.
    #[component(slot)]
    pub trailing: Option<Element>,
    /// A checkbox in the leading column, bound to whether the item is done.
    #[component(bind)]
    pub checkbox: Option<Bound<bool>>,
    /// The row was activated (a click, Enter, an assistive technology): open what it shows.
    #[component(event)]
    pub activate: Event,
}

impl ListRowProps {
    /// The props again, for a list that builds the row anew each time it scrolls into view:
    /// `None` for a row with a trailing control, which is built once.
    #[must_use]
    pub fn reusable(&self) -> Option<Self> {
        if self.trailing.is_some() {
            return None;
        }
        Some(Self {
            key: self.key.clone(),
            title: self.title.clone(),
            item: self.item.clone(),
            secondary: self.secondary.clone(),
            meta: self.meta.clone(),
            value: self.value.clone(),
            badge: self.badge.clone(),
            mark: self.mark,
            icon: self.icon,
            selected: self.selected,
            avatar_space: self.avatar_space,
            avatar: self.avatar.clone(),
            trailing: None,
            checkbox: self.checkbox.clone(),
            activate: self.activate.clone(),
        })
    }
}

impl Described for ListRow {
    type Props = ListRowProps;

    fn from_props(key: &str, props: ListRowProps) -> Self {
        let mut row = Self::new(key, String::new());
        row.apply(props);
        row
    }

    /// The texts, marks and selection change in place; a row whose leading column or trailing
    /// control changes shape is built again.
    fn set_props(&mut self, props: ListRowProps) -> bool {
        if props.trailing.is_some()
            || self.trailing.is_some()
            || leading_kind(self.leading.as_ref()) != props_kind(&props)
        {
            return false;
        }
        self.apply(props);
        self.dirty = true;
        true
    }
}

/// Which leading column a row has, to tell a change of shape from a change of content.
#[derive(PartialEq, Eq)]
enum Kind {
    None,
    Avatar,
    Space,
    Icon,
    Badge,
    Checkbox,
    Other,
}

fn leading_kind(leading: Option<&Leading>) -> Kind {
    match leading {
        None => Kind::None,
        Some(Leading::Avatar { .. }) => Kind::Avatar,
        Some(Leading::AvatarSpace) => Kind::Space,
        Some(Leading::Icon(_)) => Kind::Icon,
        Some(Leading::Badge(_)) => Kind::Badge,
        Some(Leading::Checkbox { .. }) => Kind::Checkbox,
        Some(_) => Kind::Other,
    }
}

fn props_kind(props: &ListRowProps) -> Kind {
    if props.checkbox.is_some() {
        Kind::Checkbox
    } else if props.avatar.is_some() {
        Kind::Avatar
    } else if props.avatar_space {
        Kind::Space
    } else if props
        .badge
        .as_deref()
        .is_some_and(|badge| !badge.is_empty())
    {
        Kind::Badge
    } else if props.icon.is_some() {
        Kind::Icon
    } else {
        Kind::None
    }
}

impl ListRow {
    /// Take every described part of `props`. One leading column shows, in this order of
    /// precedence: the checkbox, the avatar, the avatar's room, the badge, the icon.
    fn apply(&mut self, props: ListRowProps) {
        let ListRowProps {
            key: _,
            title,
            item: _,
            secondary,
            meta,
            value,
            badge,
            mark,
            icon,
            selected,
            avatar_space,
            avatar,
            trailing,
            checkbox,
            activate,
        } = props;
        // An empty line is no line: a row of a note without a body is one line high.
        let shown = |text: Option<String>| text.filter(|text| !text.is_empty());
        let badge = shown(badge);
        self.title = title;
        self.secondary = shown(secondary);
        self.meta = shown(meta);
        self.value = shown(value).map(|value| (value, super::RowValue::Body));
        self.mark = mark;
        self.selected = selected;
        self.leading = if let Some(bound) = checkbox {
            let checked = bound.state.get();
            Some(Leading::Checkbox {
                checked,
                on_toggle: Box::new(move |checked| {
                    bound.state.set(checked);
                    bound.edited(Value::Bool(checked));
                }),
            })
        } else if let Some(avatar) = avatar {
            let (name, identity) = avatar.name_and_identity();
            Some(Leading::Avatar {
                name,
                identity,
                image: None,
                square: avatar.square,
                glyph: avatar.glyph,
            })
        } else if avatar_space {
            Some(Leading::AvatarSpace)
        } else if let Some(badge) = badge {
            Some(Leading::Badge(badge))
        } else {
            icon.map(Leading::Icon)
        };
        if let Some(control) = trailing {
            *self = std::mem::replace(self, Self::new("", "")).trailing(control);
        }
        let label = self.title.clone();
        self.activation.set_event(activate, &label);
    }
}

/// What swiping a row across does: one command on its item, uncovered under the row as it
/// moves.
#[derive(Component, Default)]
#[component(name = "SwipeAction", role = "Button", label = "label")]
pub struct SwipeActionProps {
    /// What it does, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// The glyph the swipe uncovers: a bin for a destructive command and a check for another,
    /// unless said.
    pub icon: Option<Icon>,
    /// It takes something away: drawn as destructive, and undone rather than confirmed.
    pub destructive: bool,
    /// The swipe completed.
    #[component(event)]
    pub swipe: Event,
}

impl SwipeActionProps {
    /// The same swipe again, its event shared: a row is built anew each time it scrolls in.
    #[must_use]
    pub fn again(&self) -> Self {
        Self {
            label: self.label.clone(),
            icon: self.icon,
            destructive: self.destructive,
            swipe: self.swipe.clone(),
        }
    }
}

impl ListRow {
    /// The row with `props` as what swiping it across does.
    #[must_use]
    pub fn with_swipe(self, props: SwipeActionProps) -> Self {
        let SwipeActionProps {
            label,
            icon,
            destructive,
            swipe,
        } = props;
        let icon = icon.unwrap_or(if destructive {
            Icon::Trash
        } else {
            Icon::Check
        });
        let mut run = swipe.handler();
        self.swipe_action(icon, label, destructive, move || {
            if let Some(run) = run.as_mut() {
                run();
            }
        })
    }
}
