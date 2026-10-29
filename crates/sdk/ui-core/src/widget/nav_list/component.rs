//! [`NavList`] and its items as described components.

use crate::component::{Component, Described, Event, Value};
use crate::icons::Icon;

use super::{NavItem, NavList, NavSection};

/// The places of an app, in its sidebar: choosing one shows it. Selection follows the person's
/// choice and is reported, never kept by the app beside it.
#[derive(Component, Default)]
#[component(name = "NavList", role = "Tree", label = "title")]
pub struct NavListProps {
    /// What the places are, as a heading over them.
    pub title: Option<String>,
    /// The key of the place shown now.
    pub selected: Option<String>,
    /// A compact rail of icons that fits beside the content at every width.
    pub rail: bool,
    /// The places, in order: places, and sections of them under a heading.
    #[component(slot(default))]
    pub items: Vec<NavEntry>,
    /// A place was chosen; carries its key.
    #[component(event(value = String))]
    pub select: Event,
}

/// A group of places under a heading in a navigation list: a category of channels, a folder
/// of notebooks. A collapsible one folds away under its heading.
#[derive(Clone, Component, Debug, Default)]
#[component(name = "NavSection", role = "Group", label = "title")]
pub struct NavSectionProps {
    /// What the places are, as the heading over them.
    #[component(required, positional)]
    pub title: String,
    /// Whether its heading folds it away.
    pub collapsible: bool,
    /// The places, in order.
    #[component(slot(default))]
    pub items: Vec<NavItemProps>,
}

/// What a navigation list holds: a place, or a section of them under a heading.
#[derive(Clone, Debug)]
pub enum NavEntry {
    Item(NavItemProps),
    Section(NavSectionProps),
}

impl NavEntry {
    /// The places it holds: itself, or the section's.
    fn places(&self) -> &[NavItemProps] {
        match self {
            Self::Item(item) => std::slice::from_ref(item),
            Self::Section(section) => &section.items,
        }
    }
}

/// One place in a navigation list.
#[derive(Clone, Component, Debug, Default)]
#[component(name = "NavItem", role = "TreeItem", label = "label")]
pub struct NavItemProps {
    /// What names the place across changes: what selection reports.
    #[component(required, positional)]
    pub key: String,
    /// What the place is called.
    #[component(required, positional)]
    pub label: String,
    /// Its glyph.
    pub icon: Option<Icon>,
    /// How many things it holds.
    pub count: Option<i64>,
    /// The place was chosen: what choosing it does, beside the list's own `select`.
    #[component(event)]
    pub select: Event,
}

impl From<NavItemProps> for NavItem {
    fn from(props: NavItemProps) -> Self {
        let mut item = Self::new(props.key, props.label);
        if let Some(icon) = props.icon {
            item = item.icon(icon);
        }
        if let Some(count) = props.count {
            item = item.count(u32::try_from(count.max(0)).unwrap_or(u32::MAX));
        }
        item
    }
}

impl Described for NavList {
    type Props = NavListProps;

    fn from_props(key: &str, props: NavListProps) -> Self {
        let NavListProps {
            title,
            selected,
            rail,
            items,
            select,
        } = props;
        // What choosing each place does, by its key.
        let chosen: std::collections::HashMap<String, Event> = items
            .iter()
            .flat_map(NavEntry::places)
            .filter(|item| !item.select.is_none())
            .map(|item| (item.key.clone(), item.select.clone()))
            .collect();
        // Places outside a section run together under the list's title; each section is its
        // own, in order.
        let mut sections: Vec<NavSection> = Vec::new();
        let mut loose: Vec<NavItem> = Vec::new();
        let mut title = title;
        let mut flush = |loose: &mut Vec<NavItem>, sections: &mut Vec<NavSection>| {
            if loose.is_empty() {
                return;
            }
            let id = format!("{key}/{}", sections.len());
            let items = std::mem::take(loose);
            sections.push(match title.take() {
                Some(title) => NavSection::new(id, title, items),
                None => NavSection::untitled(id, items),
            });
        };
        for entry in items {
            match entry {
                NavEntry::Item(item) => loose.push(NavItem::from(item)),
                NavEntry::Section(section) => {
                    flush(&mut loose, &mut sections);
                    let id = format!("{key}/{}", sections.len());
                    let places = section.items.into_iter().map(NavItem::from).collect();
                    let mut built = NavSection::new(id, section.title, places);
                    if section.collapsible {
                        built = built.collapsible();
                    }
                    sections.push(built);
                }
            }
        }
        flush(&mut loose, &mut sections);
        if sections.is_empty() {
            sections.push(match title {
                Some(title) => NavSection::new(key, title, Vec::new()),
                None => NavSection::untitled(key, Vec::new()),
            });
        }
        let mut list = Self::new(sections).with_id(key);
        if let Some(selected) = selected {
            list = list.selected(&selected);
        }
        if rail {
            list = list.rail();
        }
        if select.is_none() && chosen.is_empty() {
            return list;
        }
        let mut runs: std::collections::HashMap<String, Box<dyn FnMut()>> = chosen
            .into_iter()
            .filter_map(|(key, event)| Some((key, event.handler()?)))
            .collect();
        list.on_view(key, move |id| {
            if let Some(run) = runs.get_mut(&id) {
                run();
            }
            select.fire(Some(Value::Text(id)));
        })
    }
}

/// The parts a navigation list holds.
const NAV_PARTS: [&str; 2] = ["NavItem", "NavSection"];

/// A navigation list's places and sections, in order.
impl crate::component::SlotType for Vec<NavEntry> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::OneOf(NAV_PARTS.iter().map(|name| (*name).to_owned()).collect())
    }

    fn fill(
        &mut self,
        source: &mut dyn crate::component::Source,
        slot: &str,
    ) -> Result<(), String> {
        let kinds = source.slot_kinds(slot);
        let mut read = Vec::with_capacity(kinds.len());
        let mut index = 0usize;
        source.components(slot, &mut |child| {
            let kind = kinds.get(index).cloned().flatten().unwrap_or_default();
            index = index.saturating_add(1);
            read.push(match kind.as_str() {
                "NavSection" => NavEntry::Section(
                    NavSectionProps::from_source(child).map_err(|error| error.to_string())?,
                ),
                "NavItem" => NavEntry::Item(
                    NavItemProps::from_source(child).map_err(|error| error.to_string())?,
                ),
                other => {
                    return Err(format!(
                        "holds `{other}`; a navigation list holds `ui.NavItem` and `ui.NavSection`"
                    ));
                }
            });
            Ok(())
        })?;
        *self = read;
        Ok(())
    }
}

/// [`NavItemProps`] in a slot of any number of them.
impl crate::component::SlotType for Vec<NavItemProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(NavItemProps::NAME.into())
    }

    fn fill(
        &mut self,
        source: &mut dyn crate::component::Source,
        slot: &str,
    ) -> Result<(), String> {
        let mut read = Vec::new();
        source.components(slot, &mut |child| {
            read.push(NavItemProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        *self = read;
        Ok(())
    }
}
