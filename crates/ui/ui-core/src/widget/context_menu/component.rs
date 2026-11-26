//! A menu's commands and the button that opens them as described components.

use tilo_ui_base::{component::Component, component::Described, component::Event, component::OfferedAction, component::PartType, component::Person, component::ResourceId, component::Source};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::text_render::FontChoice;
use crate::{widget::ButtonRole, widget::MenuEntry};
use tilo_ui_base::widget::Press;

use super::{MenuButton, MenuItem};

/// One command of a menu: what it shows, and what choosing it does.
#[derive(Component, Default, Clone)]
#[component(name = "MenuItem", role = "MenuItem", label = "label")]
pub struct MenuItemProps {
    /// What choosing it does, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, for recognition; the label stays its name.
    pub icon: Option<Icon>,
    /// The face the label is set in, as a family's name: a font picker sets each row in the
    /// family it offers, so a person chooses by reading it. Empty is the interface's own sans.
    pub sample: Option<String>,
    /// It takes something away: drawn as destructive, and undone rather than confirmed.
    pub destructive: bool,
    /// Whether it is the one chosen now, in a menu of choices (a sort order).
    pub checked: Option<bool>,
    /// It cannot run now (a `require` of its action does not hold). It is listed all
    /// the same, drawn as unavailable, and says `description`.
    pub disabled: bool,
    /// Why it cannot run now, in the person's words (`require`'s own): read with
    /// the row, and kept beside the label so a person sees what it waits on. The name is the
    /// one every control gives the reason it cannot be pressed now.
    pub description: Option<String>,
    /// The run of commands it stands in (`group: "Due"`), as the app names it:
    /// consecutive rows of one run fold into one row that opens them.
    pub group: Option<String>,
    /// The plain key that runs it while the window it belongs to has the keyboard, as the app's
    /// action declared it (`key: "e"`), shown at the row's trailing edge as the keyboard in use
    /// spells it.
    #[component(data)]
    pub key: Option<String>,
    /// A rule between groups of commands, which chooses nothing: a menu's `ui.MenuSeparator`.
    /// Its label and the rest say nothing while it is set.
    #[component(data)]
    pub separator: bool,
    /// The address it leads to, when it is a way to a page rather than a command: choosing it
    /// opens the address as a `ui.Link` does.
    pub link: Option<String>,
    /// It was chosen.
    #[component(event(press))]
    pub choose: Event,
}

/// The face a row's label is set in: a family by name, or the interface's own sans for none.
fn family_face(family: &str) -> FontChoice {
    let family = family.trim();
    if family.is_empty() {
        FontChoice::SansSerif
    } else {
        FontChoice::Named(family.to_owned())
    }
}

impl MenuItemProps {
    /// The command as a menu row: choosing it presses the action the view bound to it, or
    /// reports the view's own event.
    #[must_use]
    pub fn into_item(mut self) -> MenuItem {
        if self.separator {
            return MenuItem::separator();
        }
        let press = Press::of(std::mem::take(&mut self.choose)).leading_to(self.link.take());
        let props = self;
        let item = MenuItem::bound(props.label, press).with_destructive(props.destructive);
        let item = match props.icon {
            Some(icon) => item.with_icon(icon),
            None => item,
        };
        let item = match props.sample.as_deref() {
            Some(family) => item.with_sample(family_face(family)),
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
        let item = match props
            .description
            .filter(|description| !description.trim().is_empty())
        {
            Some(description) => item.with_description(description),
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
        if self.separator {
            return MenuEntry::separator();
        }
        let press = Press::of(std::mem::take(&mut self.choose)).leading_to(self.link.take());
        let props = self;
        let entry = MenuEntry::bound(props.label, press).with_destructive(props.destructive);
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
        let entry = match props
            .description
            .filter(|description| !description.trim().is_empty())
        {
            Some(description) => entry.with_description(description),
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
            description: None,
            group: None,
            key: action.key,
            link: None,
            choose: action.run,
            separator: false,
            sample: None,
        }
    }
}

/// The commands of a menu, in order.
/// The parts a menu holds, in order: its commands and the rules between their groups.
const MENU_ROW_PARTS: [&str; 2] = ["MenuItem", "MenuSeparator"];

impl PartType for Vec<MenuItemProps> {
    const MANY: bool = true;

    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::OneOf(
            MENU_ROW_PARTS
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        )
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let kinds = source.part_kinds(part);
        let mut read = Vec::with_capacity(kinds.len());
        let mut index = 0usize;
        source.components(part, &mut |child| {
            let kind = kinds.get(index).cloned().flatten().unwrap_or_default();
            index = index.saturating_add(1);
            match kind.as_str() {
                "MenuItem" => {
                    read.push(
                        MenuItemProps::from_source(child).map_err(|error| error.to_string())?,
                    );
                }
                "MenuSeparator" => read.push(MenuItemProps {
                    separator: true,
                    ..MenuItemProps::default()
                }),
                other => {
                    return Err(format!(
                        "holds `{other}`; a menu holds `ui.MenuItem` and `ui.MenuSeparator`"
                    ));
                }
            }
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
    /// Whose menu it is, as their faces in place of the label and icon: the person's own menu
    /// on a bar is their face, one person's at an avatar's full size, wearing the picture they
    /// set. The label names it to assistive technology.
    pub faces: Vec<Person>,
    /// What kind of button it is: `secondary` unless said, `plain` for an icon-only one.
    pub role: Option<ButtonRole>,
    /// Actions offered on something (`actions(r)`), each a command after `items`.
    pub actions: Vec<OfferedAction>,
    /// The resource whose published actions this menu lists, which the host fills: a bar that
    /// lists the window in front's commands names the window and leaves the commands to it.
    pub target: Option<ResourceId>,
    /// The commands, in order.
    #[component(part(default))]
    pub items: Vec<MenuItemProps>,
}

/// The button a menu button stands on: its words, its glyph, and the kind of button it is.
fn face(
    key: &str,
    label: String,
    icon: Option<Icon>,
    icon_only: bool,
    faces: Vec<Person>,
    role: Option<ButtonRole>,
) -> crate::widget::Button {
    // One person's own menu is their avatar, whole: the menu stands behind their face as it
    // stands behind any button.
    if let [only] = faces.as_slice() {
        return crate::widget::Button::new(key, "")
            .with_accessible_name(&label)
            .tooltip(label)
            .with_avatar(only.clone())
            .role(role.unwrap_or(ButtonRole::Plain));
    }
    if !faces.is_empty() {
        return crate::widget::Button::new(key, "")
            .with_accessible_name(&label)
            .tooltip(label)
            .with_faces_of(faces)
            .role(role.unwrap_or(ButtonRole::Plain));
    }
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
            faces,
            role,
            actions,
            target: _,
            items,
        } = props;
        face(key, label, icon, icon_only, faces, role).with_menu(rows(items, actions))
    }

    /// Take changed props in place: the button says the shell's new words and its menu lists the
    /// workspaces the shell has now, while the menu the person opened stays open where it stands
    /// (a bar whose workspace the shell renamed or switched under it closes nothing).
    fn set_props(&mut self, props: MenuButtonProps) -> bool {
        let MenuButtonProps {
            label,
            icon,
            icon_only,
            faces,
            role,
            actions,
            target: _,
            items,
        } = props;
        let fresh = face("", label, icon, icon_only, faces, role);
        if !self.button.take_words(&fresh) {
            return false;
        }
        self.menu.set_items(rows(items, actions));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::component::PartType;

    /// A row that is a way to a page leads there when chosen, as a link does; a row bound to a
    /// command keeps its command whatever address it also names.
    #[test]
    fn a_menu_row_leads_to_the_address_it_links_to() {
        let way = MenuItemProps {
            label: "Studio".to_owned(),
            link: Some("/w/studio/desktop".to_owned()),
            ..MenuItemProps::default()
        }
        .into_item();
        let press = way.press().expect("a row that links is chosen");
        assert!(press.is_available() && press.address().is_none());
        assert_eq!(format!("{press:?}"), "Press::Link(/w/studio/desktop)");

        let queue = tilo_ui_base::Intents::<tilo_ui_base::component::Fired>::default();
        let command = MenuItemProps {
            label: "Studio".to_owned(),
            link: Some("/w/studio/desktop".to_owned()),
            choose: Event::View(tilo_ui_base::component::Intent::new(&queue, "row", "choose")),
            ..MenuItemProps::default()
        }
        .into_item();
        command.press().expect("a bound row is chosen").run();
        assert_eq!(
            queue.take().len(),
            1,
            "the view's own event is what choosing does"
        );

        let nothing = MenuItemProps {
            label: "Studio".to_owned(),
            link: Some(String::new()),
            ..MenuItemProps::default()
        }
        .into_item();
        assert!(nothing.press().is_none(), "an empty address leads nowhere");
    }

    /// A menu button's rows take the rules between groups: a `ui.MenuSeparator` stands in the menu
    /// as a separator, and both it and a command are accepted where the rows go.
    #[test]
    fn a_menu_button_takes_a_separator_between_its_commands() {
        let accepts = Vec::<MenuItemProps>::accepts();
        let tilo_ui_base::component::Accepts::OneOf(names) = accepts else {
            panic!("a menu's rows take more than one part: {accepts:?}");
        };
        assert!(names.iter().any(|name| name == "MenuItem"));
        assert!(names.iter().any(|name| name == "MenuSeparator"));
        assert!(matches!(
            MenuItemProps {
                separator: true,
                ..MenuItemProps::default()
            }
            .into_item(),
            MenuItem::Separator
        ));
        assert!(matches!(
            MenuItemProps {
                separator: true,
                ..MenuItemProps::default()
            }
            .into_entry()
            .item(),
            MenuItem::Separator
        ));
    }
}
