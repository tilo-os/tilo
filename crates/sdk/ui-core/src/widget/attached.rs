//! What a view gives the element it stands in to do, besides drawing it: a context menu, a toolbar
//! shown on hover and focus, and a swipe. Each is a described component that *attaches*
//! ([`crate::component::Semantics::attaches`]): it draws nothing where the view puts it, and belongs to
//! the view it stands in.

use crate::component::{Accepts, Component, Event, SlotType, Source};
use crate::widget::{Element, MenuEntry, MenuItemProps, SwipeActionProps};

/// A rule between groups of a menu's commands.
#[derive(Component, Default)]
#[component(name = "MenuSeparator", role = "Splitter")]
pub struct MenuSeparatorProps {}

/// A row of emoji to react with, at the top of a menu: the ones the person uses most, then a button
/// that opens the emoji picker for any other.
#[derive(Component, Default)]
#[component(name = "EmojiBar", role = "Toolbar")]
pub struct EmojiBarProps {
    /// The emoji offered, in order: the person's most used first.
    #[component(required, positional)]
    #[component(data)]
    pub emoji: Vec<String>,
    /// A last button opens the emoji picker, for an emoji not offered.
    pub more: bool,
    /// An emoji was picked: carries it.
    #[component(event(value = String))]
    pub pick: Event,
}

pub enum MenuPart {
    Item(MenuItemProps),
    Separator,
    Emoji(EmojiBarProps),
}

impl std::fmt::Debug for MenuPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Item(item) => write!(f, "Item({:?})", item.label),
            Self::Separator => f.write_str("Separator"),
            Self::Emoji(bar) => write!(f, "Emoji({:?})", bar.emoji),
        }
    }
}

impl MenuPart {
    /// The same part again, its events shared: a menu is built anew each time it opens.
    #[must_use]
    pub fn again(&self) -> Self {
        match self {
            Self::Item(item) => Self::Item(MenuItemProps {
                label: item.label.clone(),
                icon: item.icon,
                destructive: item.destructive,
                checked: item.checked,
                disabled: item.disabled,
                reason: item.reason.clone(),
                group: item.group.clone(),
                key: item.key.clone(),
                choose: item.choose.clone(),
            }),
            Self::Separator => Self::Separator,
            Self::Emoji(bar) => Self::Emoji(EmojiBarProps {
                emoji: bar.emoji.clone(),
                more: bar.more,
                pick: bar.pick.clone(),
            }),
        }
    }

    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Item(item) => Some(&item.label),
            Self::Separator | Self::Emoji(_) => None,
        }
    }

    fn invokes(&self) -> Option<&tilo_actions::ActionAddress> {
        match self {
            Self::Item(item) => item.choose.invokes(),
            Self::Separator | Self::Emoji(_) => None,
        }
    }
}

const MENU_PARTS: [&str; 3] = ["MenuItem", "MenuSeparator", "EmojiBar"];

/// A menu's parts, in order: commands, separators and an emoji bar.
impl SlotType for Vec<MenuPart> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::OneOf(MENU_PARTS.iter().map(|name| (*name).to_owned()).collect())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let kinds = source.slot_kinds(slot);
        let mut parts = Vec::with_capacity(kinds.len());
        let mut index = 0usize;
        source.components(slot, &mut |child| {
            let kind = kinds.get(index).cloned().flatten().unwrap_or_default();
            index = index.saturating_add(1);
            parts.push(match kind.as_str() {
                "MenuItem" => {
                    MenuPart::Item(MenuItemProps::from_source(child).map_err(|e| e.to_string())?)
                }
                "MenuSeparator" => MenuPart::Separator,
                "EmojiBar" => {
                    MenuPart::Emoji(EmojiBarProps::from_source(child).map_err(|e| e.to_string())?)
                }
                other => {
                    return Err(format!(
                        "holds `{other}`; a menu holds `ui.MenuItem`, `ui.MenuSeparator` and `ui.EmojiBar`"
                    ));
                }
            });
            Ok(())
        })?;
        *self = parts;
        Ok(())
    }
}

/// A context menu of the view it stands in: a right-click, the Menu key or a long press opens it.
#[derive(Component, Default)]
#[component(name = "ContextMenu", role = "Menu", attaches)]
pub struct ContextMenuProps {
    /// The menu's content, in order: commands, separators and an emoji bar.
    #[component(slot(default))]
    pub items: Vec<MenuPart>,
}

/// Controls shown over the trailing edge of the view they stand in while it is hovered or holds focus:
/// quick reactions, Reply.
#[derive(Component, Default)]
#[component(name = "HoverToolbar", role = "Toolbar", attaches)]
pub struct HoverToolbarProps {
    /// The controls, in order: buttons, an emoji picker.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

/// What swiping the view it stands in across does: it follows the finger, uncovers the command and runs
/// it when let go past the threshold, or springs back.
#[derive(Component, Default)]
#[component(name = "Swipe", role = "Group", attaches)]
pub struct SwipeProps {
    /// What swiping from the leading edge towards the trailing edge does.
    #[component(slot)]
    pub leading: Option<SwipeActionProps>,
    /// What swiping from the trailing edge towards the leading edge does.
    #[component(slot)]
    pub trailing: Option<SwipeActionProps>,
}

impl SwipeProps {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leading.is_none() && self.trailing.is_none()
    }
}

#[derive(Default)]
pub struct Attachments {
    /// The context menu's content, the view's own order.
    pub menu: Vec<MenuPart>,
    pub toolbar: Vec<Element>,
    pub swipe: SwipeProps,
}

impl std::fmt::Debug for Attachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachments")
            .field("menu", &self.menu)
            .field("toolbar", &self.toolbar.len())
            .field("leading", &self.swipe.leading.as_ref().map(|s| &s.label))
            .field("trailing", &self.swipe.trailing.as_ref().map(|s| &s.label))
            .finish()
    }
}

impl Attachments {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.menu.is_empty() && self.toolbar.is_empty() && self.swipe.is_empty()
    }
}

const ATTACHMENTS: [&str; 3] = ["ContextMenu", "HoverToolbar", "Swipe"];

impl SlotType for Attachments {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::OneOf(ATTACHMENTS.iter().map(|name| (*name).to_owned()).collect())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let kinds = source.slot_kinds(slot);
        let mut index = 0usize;
        let mut swipes = 0usize;
        let attached = &mut *self;
        source.components(slot, &mut |child| {
            let kind = kinds.get(index).cloned().flatten().unwrap_or_default();
            index = index.saturating_add(1);
            match kind.as_str() {
                "ContextMenu" => {
                    let menu = ContextMenuProps::from_source(child).map_err(|e| e.to_string())?;
                    attached.menu.extend(menu.items);
                }
                "HoverToolbar" => {
                    let bar = HoverToolbarProps::from_source(child).map_err(|e| e.to_string())?;
                    attached.toolbar.extend(bar.content);
                }
                "Swipe" => {
                    swipes = swipes.saturating_add(1);
                    if swipes > 1 {
                        return Err("holds two swipes; a view swipes one way per edge".to_owned());
                    }
                    attached.swipe = SwipeProps::from_source(child).map_err(|e| e.to_string())?;
                }
                other => {
                    return Err(format!(
                        "holds `{other}`; a view attaches `ui.ContextMenu`, `ui.HoverToolbar` and `ui.Swipe`"
                    ));
                }
            }
            Ok(())
        })?;
        Ok(())
    }
}

/// The menu of an item: the view's own content first, as it placed it; then, after a separator,
/// each command the item offers (`offered`, its allowed actions) that the view did not place, so
/// every action stays reachable from the menu.
#[must_use]
pub fn item_menu(placed: Vec<MenuPart>, offered: Vec<MenuPart>) -> Vec<MenuPart> {
    let taken: Vec<tilo_actions::ActionAddress> = placed
        .iter()
        .filter_map(MenuPart::invokes)
        .cloned()
        .collect();
    let named: Vec<&str> = placed
        .iter()
        .filter(|part| part.invokes().is_none())
        .filter_map(MenuPart::label)
        .collect();
    let rest: Vec<MenuPart> = offered
        .into_iter()
        .filter(|part| {
            !part
                .invokes()
                .is_some_and(|address| taken.contains(address))
                && !part.label().is_some_and(|label| named.contains(&label))
        })
        .collect();
    let mut menu = placed;
    let commands = rest.iter().any(|part| !matches!(part, MenuPart::Separator));
    if commands {
        let ends_apart = matches!(menu.last(), None | Some(MenuPart::Separator));
        if !ends_apart {
            menu.push(MenuPart::Separator);
        }
        menu.extend(rest);
    }
    menu
}

#[must_use]
pub fn menu_entries(parts: Vec<MenuPart>) -> Vec<MenuEntry> {
    let mut entries: Vec<MenuEntry> = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            MenuPart::Item(item) => entries.push(item.into_entry()),
            MenuPart::Separator => {
                let after_command = entries
                    .last()
                    .is_some_and(|entry| !entry.item().is_separator());
                if after_command {
                    entries.push(MenuEntry::separator());
                }
            }
            MenuPart::Emoji(bar) => entries.push(MenuEntry::emoji_bar(bar)),
        }
    }
    while entries
        .last()
        .is_some_and(|entry| entry.item().is_separator())
    {
        entries.pop();
    }
    MenuEntry::fold_groups(entries)
}

#[cfg(test)]
#[path = "attached_tests.rs"]
mod tests;
