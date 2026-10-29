//! A menu's commands and the button that opens them as described components.

use std::rc::Rc;

use crate::component::{Component, Described, Event, OfferedAction, SlotType, Source};
use crate::icons::Icon;
use crate::widget::{ButtonRole, MenuEntry};

use super::{MenuButton, MenuItem};

/// One command of a menu: what it shows, and what choosing it does.
#[derive(Component, Default)]
#[component(name = "MenuItem", role = "MenuItem", label = "label")]
pub struct MenuItemProps {
    /// What choosing it does, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, for recognition; the label stays its name.
    pub icon: Option<Icon>,
    /// It takes something away: drawn as destructive, and undone rather than confirmed.
    pub destructive: bool,
    /// Whether it is the one chosen now, in a menu of choices (a sort order).
    pub checked: Option<bool>,
    /// It was chosen.
    #[component(event)]
    pub choose: Event,
}

impl MenuItemProps {
    /// What choosing it runs, taken out of the props.
    fn take_run(&mut self) -> Rc<dyn Fn()> {
        std::mem::take(&mut self.choose)
            .command()
            .unwrap_or_else(|| Rc::new(|| {}))
    }

    /// The command as a menu row.
    #[must_use]
    pub fn into_item(mut self) -> MenuItem {
        let run = self.take_run();
        let props = self;
        let item = if props.destructive {
            MenuItem::destructive_action(props.label, move || run())
        } else {
            MenuItem::action(props.label, move || run())
        };
        let item = match props.icon {
            Some(icon) => item.with_icon(icon),
            None => item,
        };
        match props.checked {
            Some(checked) => item.with_checked(checked),
            None => item,
        }
    }

    /// The command as an entry of a context menu.
    #[must_use]
    pub fn into_entry(mut self) -> MenuEntry {
        let run = self.take_run();
        let props = self;
        let entry = if props.destructive {
            MenuEntry::destructive(props.label, move || run())
        } else {
            MenuEntry::new(props.label, move || run())
        };
        match props.icon {
            Some(icon) => entry.with_icon(icon),
            None => entry,
        }
    }
}

impl From<OfferedAction> for MenuItemProps {
    /// An action offered to the person as a command: choosing it runs the action.
    fn from(action: OfferedAction) -> Self {
        Self {
            label: action.label,
            icon: action.icon,
            destructive: action.destructive,
            checked: action.on,
            choose: action.run,
        }
    }
}

/// The commands of a menu, in order.
impl SlotType for Vec<MenuItemProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(MenuItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut read = Vec::new();
        source.components(slot, &mut |child| {
            read.push(MenuItemProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        *self = read;
        Ok(())
    }
}

/// A button that opens a menu of commands under it: a header's More button, a sort menu.
#[derive(Component, Default)]
#[component(name = "MenuButton", role = "Button", label = "label")]
pub struct MenuButtonProps {
    /// What the menu holds, in a word or two. An icon-only button is still named by it.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, or instead of it when `icon_only`.
    pub icon: Option<Icon>,
    /// Show only the icon, the label naming the button to assistive technology.
    pub icon_only: bool,
    /// What kind of button it is: `secondary` unless said, `plain` for an icon-only one.
    pub role: Option<ButtonRole>,
    /// Actions offered on something (`actions(r)`), each a command after `items`.
    pub actions: Vec<OfferedAction>,
    /// The commands, in order.
    #[component(slot(default))]
    pub items: Vec<MenuItemProps>,
}

impl Described for MenuButton {
    type Props = MenuButtonProps;

    fn from_props(key: &str, props: MenuButtonProps) -> Self {
        let MenuButtonProps {
            label,
            icon,
            icon_only,
            role,
            actions,
            items,
        } = props;
        let button = match icon {
            Some(icon) if icon_only => crate::widget::Button::icon_only(key, icon, &label),
            Some(icon) => crate::widget::Button::new(key, &label)
                .with_icon(icon)
                .role(role.unwrap_or_default()),
            None => crate::widget::Button::new(key, &label).role(role.unwrap_or_default()),
        };
        button.with_menu(
            items
                .into_iter()
                .chain(actions.into_iter().map(MenuItemProps::from))
                .map(MenuItemProps::into_item)
                .collect(),
        )
    }
}
