//! [`ListRow`] as a described component.

use crate::component::{Bound, Component, Described, Event, ResourceId, Value};
use crate::icons::Icon;
use crate::widget::{AvatarProps, Element};

use super::{Leading, ListRow};
use crate::widget::Tone;

/// A row of a list: what an item is, in one line, with the start of its content under it
/// and a quiet fact on its trailing edge.
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
    /// A quiet word before the title, on the same line and in the weaker ink: where an item
    /// came from, what it answers.
    pub prefix: Option<String>,
    /// The resource the row shows, which activating the row opens.
    pub item: Option<ResourceId>,
    /// The line under the title: the start of the item's content, the last message.
    pub secondary: Option<String>,
    /// A line of the row's own under its text: what waits on the item and is not its
    /// content, as a proposal about it reads ("Suggested: move this to the end").
    pub note: Option<String>,
    /// When it happened or is due: the first fact of the row's facts column.
    pub meta: Option<String>,
    /// The tone the trailing fact reads in, where the item it states has one: a day gone by
    /// is overdue.
    pub meta_tone: Tone,
    /// The item's figure among its facts, as a setting shows it.
    pub value: Option<String>,
    /// A short code in the leading column: a method, a kind.
    pub badge: Option<String>,
    /// A mark on the row's edge, after every other fact: pinned, starred, an item's priority.
    pub mark: Option<Icon>,
    /// The tone the mark and the row's checkbox ring read in: the item's priority, a theme
    /// role rather than a colour.
    pub tone: Tone,
    /// A glyph in the leading column: what kind of item it is.
    pub icon: Option<Icon>,
    /// Whether the row is selected in its list.
    pub selected: bool,
    /// Who the item is: their face among its facts, between when it is due and how far it
    /// has got.
    #[component(slot)]
    pub avatar: Option<AvatarProps>,
    /// A control on the trailing edge, which moves under the text when the row is narrow.
    #[component(slot)]
    pub trailing: Option<Element>,
    /// A checkbox in the leading column, bound to whether the item is done.
    #[component(bind)]
    pub checkbox: Option<Bound<bool>>,
    /// The checkbox was pressed: what a check is, in the app's words.
    #[component(event(press))]
    pub check: Event,
    /// The pace the row's own motion plays at: its mark coming and going, and the row
    /// arriving, leaving and changing places in its list.
    pub motion: Option<crate::motion::Feel>,
    /// The row stands dense: one quiet line at the caption's size with the small glyph, as
    /// tall as that line and its inset, for a reference under content rather than a list of
    /// items.
    pub dense: bool,
    /// The row was activated (a click, Enter, an assistive technology): open what it shows.
    #[component(event(press))]
    pub activate: Event,
    /// The row reads as subordinate to the rows around it: its title in the weaker ink, as
    /// an outline's quieter branch does.
    pub quiet: bool,
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
            prefix: self.prefix.clone(),
            item: self.item.clone(),
            secondary: self.secondary.clone(),
            note: self.note.clone(),
            meta: self.meta.clone(),
            meta_tone: self.meta_tone,
            value: self.value.clone(),
            badge: self.badge.clone(),
            mark: self.mark,
            tone: self.tone,
            icon: self.icon,
            selected: self.selected,
            avatar: self.avatar.clone(),
            trailing: None,
            checkbox: self.checkbox.clone(),
            check: self.check.clone(),
            motion: self.motion,
            dense: self.dense,
            activate: self.activate.clone(),
            quiet: self.quiet,
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

    /// The texts, marks and selection change in place; a row whose leading column or
    /// trailing control changes shape is built again.
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
    Icon,
    Badge,
    Checkbox,
    Other,
}

fn leading_kind(leading: Option<&Leading>) -> Kind {
    match leading {
        None => Kind::None,
        Some(Leading::Icon(_)) => Kind::Icon,
        Some(Leading::Badge(_)) => Kind::Badge,
        Some(Leading::Checkbox { .. }) => Kind::Checkbox,
        Some(_) => Kind::Other,
    }
}

fn props_kind(props: &ListRowProps) -> Kind {
    if props.checkbox.is_some() {
        Kind::Checkbox
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
    /// Take every described part of `props`.
    #[allow(
        clippy::too_many_lines,
        reason = "the props of a row are taken into the row in one place, in their own order"
    )]
    fn apply(&mut self, props: ListRowProps) {
        let ListRowProps {
            key: _,
            title,
            prefix,
            item: _,
            secondary,
            note,
            meta,
            meta_tone,
            value,
            badge,
            mark,
            tone,
            icon,
            selected,
            avatar,
            trailing,
            checkbox,
            check,
            motion,
            dense,
            activate,
            quiet,
        } = props;
        // An empty line is no line: a row of a note without a body is one line high.
        let shown = |text: Option<String>| text.filter(|text| !text.is_empty());
        let badge = shown(badge);
        self.title = title;
        self.prefix = shown(prefix);
        self.secondary = shown(secondary);
        self.note = shown(note);
        self.meta = shown(meta);
        self.meta_spans = match shown(self.meta.clone()) {
            Some(text) if meta_tone != Tone::Neutral => vec![(text, meta_tone)],
            _ => Vec::new(),
        };
        self.value = shown(value).map(|value| (value, super::RowValue::Body));
        self.mark = mark;
        self.mark_tone = tone;
        self.avatar = avatar.map(|avatar| {
            let (name, identity) = avatar.name_and_identity();
            super::FactAvatar {
                name,
                identity,
                image: None,
                square: avatar.square,
                glyph: avatar.glyph,
            }
        });
        self.selected = selected;
        self.quiet = quiet;
        self.feel = motion.unwrap_or(crate::motion::Feel::Standard);
        self.dense = dense;
        self.check_binding = None;
        self.check_action = match check.clone() {
            // The app's own action for what a check is: the box holds it, so pressing the box
            // and an agent's invocation of the action are the one route.
            Event::Action(binding) => Some(crate::widget::ActionEmitter::new(binding)),
            Event::None | Event::View(_) => None,
        };
        self.leading = if let Some(bound) = checkbox {
            // The box follows the value it is bound to: one set elsewhere (another device, an
            // agent, an undo) shows in the row rather than only in the state behind it.
            let (checked, binding) = crate::widget::binding::local(&bound.state);
            self.check_binding = Some(binding);
            let on_toggle: Box<dyn FnMut(bool)> = if check.is_none() {
                Box::new(move |checked| {
                    bound.state.set(checked);
                    bound.edited(Value::Bool(checked));
                })
            } else {
                // The app says what a press means; the box shows what it did on the next frame.
                Box::new(move |_| check.fire(None))
            };
            Some(Leading::Checkbox {
                checked,
                on_toggle,
                icon,
            })
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
    /// The glyph the swipe uncovers: a bin for a destructive command and a check for
    /// another, unless said.
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
