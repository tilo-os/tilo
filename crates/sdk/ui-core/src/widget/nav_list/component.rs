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
    /// The places, in order.
    #[component(slot(default))]
    pub items: Vec<NavItemProps>,
    /// A place was chosen; carries its key.
    #[component(event(value = String))]
    pub select: Event,
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
            .filter(|item| !item.select.is_none())
            .map(|item| (item.key.clone(), item.select.clone()))
            .collect();
        let items: Vec<NavItem> = items.into_iter().map(NavItem::from).collect();
        let section = match title {
            Some(title) => NavSection::new(key, title, items),
            None => NavSection::untitled(key, items),
        };
        let mut list = Self::new(vec![section]).with_id(key);
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
        list.on_select(move |id| {
            if let Some(run) = runs.get_mut(&id) {
                run();
            }
            select.fire(Some(Value::Text(id)));
        })
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
