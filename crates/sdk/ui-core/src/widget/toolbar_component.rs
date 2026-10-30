//! [`Toolbar`] and its parts as described components.

use crate::component::{Accepts, Component, Described, Event, SlotType, Source};
use crate::icons::Icon;
use crate::widget::MenuItemProps;

use super::{Orientation, Toolbar, ToolbarItem};

/// A strip of commands: plain buttons in a row, groups split by hairlines, toggles that stand
/// lifted while on. However narrow the window, every command stays reachable: those that do not
/// fit go to a More menu at the end, the least important first.
#[derive(Component, Default)]
#[component(name = "Toolbar", role = "Toolbar")]
pub struct ToolbarProps {
    /// Its commands, separators and spacers, in order.
    #[component(slot(default))]
    pub items: Vec<ToolbarPart>,
    /// Stand the commands in a column rather than a row.
    pub vertical: bool,
}

/// One command of a toolbar: pressing it does one thing, or opens its menu.
#[derive(Component)]
#[component(name = "ToolbarButton", role = "Button", label = "label")]
pub struct ToolbarButtonProps {
    /// The command's stable key.
    #[component(key)]
    pub key: String,
    /// What it does, in a word or two. A command with a glyph shows only the glyph, the label
    /// naming it to assistive technology, in its tooltip and in the More menu.
    #[component(required, positional)]
    pub label: String,
    /// A glyph instead of the label.
    pub icon: Option<Icon>,
    /// A command that is on or off (bold, a panel shown), and whether it is on: on, it stands
    /// lifted, and it is a checked row in the More menu.
    pub toggled: Option<bool>,
    /// Whether it can be pressed now. A command with nothing to do never can.
    pub enabled: bool,
    /// A dot at its corner: something new behind it.
    pub dot: bool,
    /// How long it stays in the row where the window is narrow: higher stays longer, and of
    /// equal ones the last goes to the More menu first.
    pub priority: i64,
    /// It goes with the command before it (zoom out, zoom in): the two go to the More menu
    /// together, never one alone.
    pub joined: bool,
    /// The commands of a menu it opens instead of doing one thing: Sort, Insert, View.
    #[component(slot)]
    pub menu: Vec<MenuItemProps>,
    /// It was pressed (a click, Enter or Space, the More menu, an assistive technology).
    #[component(event)]
    pub press: Event,
}

impl Default for ToolbarButtonProps {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            icon: None,
            toggled: None,
            enabled: true,
            dot: false,
            priority: 0,
            joined: false,
            menu: Vec::new(),
            press: Event::None,
        }
    }
}

/// A hairline between groups of a toolbar's commands. It never stands at the start or the end
/// of what is shown.
#[derive(Component, Default)]
#[component(name = "ToolbarSeparator", role = "Separator")]
pub struct ToolbarSeparatorProps {}

/// The room left in a toolbar, between the commands before it and those after it, which it
/// pushes to the far end.
#[derive(Component, Default)]
#[component(name = "ToolbarSpacer", role = "Generic")]
pub struct ToolbarSpacerProps {}

/// One part of a toolbar: a command, a separator or a spacer.
pub enum ToolbarPart {
    Button(ToolbarButtonProps),
    Separator,
    Spacer,
}

/// The parts a toolbar holds.
const TOOLBAR_PARTS: [&str; 3] = ["ToolbarButton", "ToolbarSeparator", "ToolbarSpacer"];

/// A toolbar's parts, in order: commands, separators and spacers.
impl SlotType for Vec<ToolbarPart> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::OneOf(
            TOOLBAR_PARTS
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        )
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let kinds = source.slot_kinds(slot);
        let mut parts = Vec::with_capacity(kinds.len());
        let mut index = 0usize;
        source.components(slot, &mut |child| {
            let kind = kinds.get(index).cloned().flatten().unwrap_or_default();
            index = index.saturating_add(1);
            parts.push(match kind.as_str() {
                "ToolbarButton" => ToolbarPart::Button(
                    ToolbarButtonProps::from_source(child).map_err(|e| e.to_string())?,
                ),
                "ToolbarSeparator" => ToolbarPart::Separator,
                "ToolbarSpacer" => ToolbarPart::Spacer,
                other => {
                    return Err(format!(
                        "holds `{other}`; a toolbar holds `ui.ToolbarButton`, \
                         `ui.ToolbarSeparator` and `ui.ToolbarSpacer`"
                    ));
                }
            });
            Ok(())
        })?;
        *self = parts;
        Ok(())
    }
}

/// `bar` with the command of `props` at its end.
fn push_command(bar: Toolbar, props: ToolbarButtonProps) -> Toolbar {
    let ToolbarButtonProps {
        key,
        label,
        icon,
        toggled,
        enabled,
        dot,
        priority,
        joined,
        menu,
        press,
    } = props;
    let mut bar = if menu.is_empty() {
        let enabled = enabled && !press.is_none();
        let run: Box<dyn FnMut()> = match &press {
            Event::View(intent) => Box::new(intent.sender()),
            Event::None | Event::Action(_) => Box::new(|| {}),
        };
        let bar = match (icon, toggled) {
            (Some(icon), Some(on)) => bar.push_icon_toggle(icon, &label, on, run),
            (Some(icon), None) => bar.push_icon(icon, &label, run),
            (None, Some(on)) => bar.push_toggle(&label, on, run),
            (None, None) => bar.push_button(&label, run),
        };
        let bar = match press {
            Event::None => bar,
            Event::View(_) => bar.with_view(&key),
            Event::Action(binding) => bar.with_action(binding),
        };
        bar.with_enabled(enabled)
    } else {
        let items = menu.into_iter().map(MenuItemProps::into_item).collect();
        let mut bar = bar.push_menu(&label, items).with_enabled(enabled);
        if let Some(command) = bar.last_command() {
            command.icon = icon;
        }
        bar
    };
    if let Some(command) = bar.last_command() {
        command.dot = dot;
        command.joined = joined;
        command.priority = i32::try_from(priority).unwrap_or(0);
    }
    bar
}

impl Described for Toolbar {
    type Props = ToolbarProps;

    fn from_props(_key: &str, props: ToolbarProps) -> Self {
        let orientation = if props.vertical {
            Orientation::Vertical
        } else {
            Orientation::Horizontal
        };
        props
            .items
            .into_iter()
            .fold(Self::new(orientation), |bar, part| match part {
                ToolbarPart::Button(command) => push_command(bar, command),
                ToolbarPart::Separator => bar.push_separator(),
                ToolbarPart::Spacer => bar.push_spacer(),
            })
    }

    /// The same commands take a new label, toggle, dot, enabled state or menu in place, so a
    /// toggle flipped by its own press stays where it is.
    fn set_props(&mut self, props: ToolbarProps) -> bool {
        let vertical = self.orientation == Orientation::Vertical;
        if props.vertical != vertical || props.items.len() != self.items.len() {
            return false;
        }
        let same_shape =
            props
                .items
                .iter()
                .zip(&self.items)
                .all(|(part, held)| match (part, held) {
                    (ToolbarPart::Button(props), ToolbarItem::Button(command)) => {
                        props.icon == command.icon
                            && props.toggled.is_some() == command.active.is_some()
                            && props.menu.is_empty() == command.menu.is_none()
                            && i64::from(command.priority) == props.priority
                            && props.joined == command.joined
                    }
                    (ToolbarPart::Separator, ToolbarItem::Separator)
                    | (ToolbarPart::Spacer, ToolbarItem::Spacer) => true,
                    _ => false,
                });
        if !same_shape {
            return false;
        }
        for (index, part) in props.items.into_iter().enumerate() {
            let ToolbarPart::Button(command) = part else {
                continue;
            };
            let has_menu = !command.menu.is_empty();
            let enabled = command.enabled && (has_menu || !command.press.is_none());
            self.set_label(index, &command.label);
            if let Some(on) = command.toggled {
                self.set_active(index, on);
            }
            self.set_enabled(index, enabled);
            self.set_dot(index, command.dot);
            if has_menu {
                let items = command
                    .menu
                    .into_iter()
                    .map(MenuItemProps::into_item)
                    .collect();
                self.set_menu(index, items);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(on: bool) -> ToolbarProps {
        ToolbarProps {
            items: vec![ToolbarPart::Button(ToolbarButtonProps {
                key: "bold".into(),
                label: "Bold".into(),
                toggled: Some(on),
                press: Event::View(crate::component::Intent::new(
                    &crate::Intents::default(),
                    "bold",
                    "press",
                )),
                ..ToolbarButtonProps::default()
            })],
            vertical: false,
        }
    }

    #[test]
    fn a_toggle_flipped_by_its_press_is_taken_in_place() {
        let mut toolbar = Toolbar::from_props("format", bold(false));
        assert!(toolbar.set_props(bold(true)));
        assert_eq!(toolbar.command(0).and_then(|c| c.active), Some(true));
    }
}
