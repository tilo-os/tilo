//! A menu's commands and the button that opens them as described components.

use std::rc::Rc;

use crate::component::{Component, Described, Event, OfferedAction, ResourceId, SlotType, Source};
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
    /// It cannot run now (a `require` of its action does not hold). It is listed all
    /// the same, drawn as unavailable, and says `reason`.
    pub disabled: bool,
    /// Why it cannot run now, in the person's words (`require`'s own): read with
    /// the row, and kept beside the label so a person sees what it waits on.
    pub reason: Option<String>,
    /// The run of commands it stands in (`group: "Due"`), as the app names it:
    /// consecutive rows of one run fold into one row that opens them.
    pub group: Option<String>,
    /// The plain key that runs it while the window it belongs to has the keyboard, as the app's
    /// action declared it (`key: "e"`), shown at the row's trailing edge as the keyboard in use
    /// spells it.
    #[component(data)]
    pub key: Option<String>,
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
        let item = match props.checked {
            Some(checked) => item.with_checked(checked),
            None => item,
        };
        let item = item.with_enabled(!props.disabled);
        // The app's declared key shows at the row's trailing edge, as the keyboard in use
        // spells it.
        let item = match props.key {
            Some(key) => item.with_shortcut(key),
            None => item,
        };
        let item = match props.reason.filter(|reason| !reason.trim().is_empty()) {
            Some(reason) => item.with_description(reason),
            None => item,
        };
        match props.group {
            Some(group) => item.with_group(group),
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
        let entry = match props.icon {
            Some(icon) => entry.with_icon(icon),
            None => entry,
        };
        let entry = entry.with_enabled(!props.disabled);
        // The app's declared key shows at the row's trailing edge, as the keyboard in use
        // spells it.
        let entry = match props.key {
            Some(key) => entry.with_shortcut(key),
            None => entry,
        };
        let entry = match props.reason.filter(|reason| !reason.trim().is_empty()) {
            Some(reason) => entry.with_description(reason),
            None => entry,
        };
        match props.group {
            Some(group) => entry.with_group(group),
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
            disabled: false,
            reason: None,
            group: None,
            key: action.key,
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
    /// The resource whose published actions this menu lists, which the host fills: a bar that
    /// lists the window in front's commands names the window and leaves the commands to it.
    pub subject: Option<ResourceId>,
    /// The commands, in order.
    #[component(slot(default))]
    pub items: Vec<MenuItemProps>,
}

/// The button a menu button stands on: its words, its glyph, and the kind of button it is.
fn face(
    key: &str,
    label: String,
    icon: Option<Icon>,
    icon_only: bool,
    role: Option<ButtonRole>,
) -> crate::widget::Button {
    match icon {
        Some(icon) if icon_only => crate::widget::Button::icon_only(key, icon, &label),
        Some(icon) => crate::widget::Button::new(key, &label)
            .with_icon(icon)
            .role(role.unwrap_or_default()),
        None => crate::widget::Button::new(key, &label).role(role.unwrap_or_default()),
    }
}

/// The rows a menu button lists: its commands, then the actions offered on something.
fn rows(items: Vec<MenuItemProps>, actions: Vec<OfferedAction>) -> Vec<MenuItem> {
    items
        .into_iter()
        .chain(actions.into_iter().map(MenuItemProps::from))
        .map(MenuItemProps::into_item)
        .collect()
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
            subject: _,
            items,
        } = props;
        face(key, label, icon, icon_only, role).with_menu(rows(items, actions))
    }

    /// Take changed props in place: the button says the desk's new words and its menu lists the
    /// workspaces the desk has now, while the menu the person opened stays open where it stands
    /// (a bar whose workspace the desk renamed or switched under it closes nothing).
    fn set_props(&mut self, props: MenuButtonProps) -> bool {
        let MenuButtonProps {
            label,
            icon,
            icon_only,
            role,
            actions,
            subject: _,
            items,
        } = props;
        let fresh = face("", label, icon, icon_only, role);
        if !self.button.take_words(&fresh) {
            return false;
        }
        self.menu.set_items(rows(items, actions));
        true
    }
}
