//! [`NavList`] and its items as described components.

use crate::component::{Component, Described, Event, OfferedAction, Person, ResourceId, Value};
use crate::icons::Icon;

use super::{NavItem, NavList, NavSection};

/// A sidebar of places whose choice is reported to the app: choosing one shows it.
#[derive(Component, Default)]
#[component(name = "NavList", role = "Tree", label = "title")]
pub struct NavListProps {
    /// The heading over the places.
    pub title: Option<String>,
    /// The key of the place shown now.
    #[component(data)]
    #[component(identity)]
    pub selected: Option<String>,
    /// A compact rail of icons that fits beside the content at every width.
    pub rail: bool,
    /// The places, in order, and sections of them under a heading.
    #[component(slot(default))]
    pub items: Vec<NavEntry>,
    /// A place was chosen: its key is the value.
    #[component(event(value = String))]
    pub select: Event,
}

/// A group of places under a heading (a category of channels, a folder of notebooks); a
/// collapsible one folds away.
#[derive(Clone, Component, Debug, Default)]
#[component(name = "NavSection", role = "Group", label = "title")]
pub struct NavSectionProps {
    /// The heading over the places.
    #[component(required, positional)]
    pub title: String,
    /// Whether its heading folds it away.
    pub collapsible: bool,
    /// The action that makes another place in the group ("New project"), drawn at the end of
    /// the heading.
    pub add: Option<OfferedAction>,
    /// The places, in order.
    #[component(slot(default))]
    pub items: Vec<NavItemProps>,
}

#[derive(Clone, Debug)]
pub enum NavEntry {
    Item(NavItemProps),
    Section(NavSectionProps),
}

impl NavEntry {
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
    /// What names the place across changes, and what its selection reports.
    #[component(required, positional)]
    #[component(data)]
    #[component(identity)]
    pub key: String,
    /// What the place is called.
    #[component(required, positional)]
    pub label: String,
    /// Its glyph.
    pub icon: Option<Icon>,
    /// How many things it has: a quiet count in tertiary ink.
    pub count: Option<i64>,
    /// How many things wait for attention (unread, mentions): the list's one filled badge in
    /// the person's colour, nothing at zero.
    pub live: Option<i64>,
    /// Something new waits behind the place: its label stands out at rest.
    pub unread: bool,
    /// How many things behind the place name the person: more specific than the unread count,
    /// it replaces the badge while there are any.
    pub mentions: Option<i64>,
    /// The resource the place shows, when it is one (a channel): the host says in `live` how
    /// much of it is unseen, and the label lifts while anything is.
    pub item: Option<ResourceId>,
    /// The person the place shows, when it is one (a direct conversation): their avatar and
    /// presence ring in the icon's place.
    pub person: Option<Person>,
    /// The place was chosen: what choosing it does, beside the list's `select`.
    #[component(event(press))]
    pub select: Event,
}

impl From<NavItemProps> for NavItem {
    fn from(props: NavItemProps) -> Self {
        let mut item = Self::new(props.key, props.label);
        if let Some(item_of) = props.item {
            item = item.resource(item_of.0);
        }
        if let Some(icon) = props.icon {
            item = item.icon(icon);
        }
        if let Some(person) = props.person {
            item = item.person(&person.identity, None, false);
            item = item.person_face(person.name, None);
        }
        if let Some(mentions) = props.mentions.filter(|mentions| *mentions > 0) {
            item = item
                .live_count(u32::try_from(mentions).unwrap_or(u32::MAX))
                .unread(true);
        } else if let Some(live) = props.live {
            item = item.live_count(u32::try_from(live.max(0)).unwrap_or(u32::MAX));
        } else if let Some(count) = props.count {
            item = item.count(u32::try_from(count.max(0)).unwrap_or(u32::MAX));
        }
        if props.unread {
            item = item.unread(true);
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
        let placed = place(key, title, items, select);
        let mut list = Self::new(placed.sections.clone()).with_id(key);
        if let Some(selected) = selected {
            list = list.selected(&selected);
        }
        if rail {
            list = list.rail();
        }
        list.report(key, &placed);
        list
    }

    /// The places, their headings and what choosing one does change where the list stands: the
    /// rows whose content changed are built again, and every other keeps its place, its focus
    /// and what was folded.
    fn set_props(&mut self, props: NavListProps) -> bool {
        let NavListProps {
            title,
            selected,
            rail,
            items,
            select,
        } = props;
        let placed = place(&self.key, title, items, select);
        self.set_sections(placed.sections.clone());
        self.set_selected(selected.as_deref());
        self.set_rail_rows(rail);
        self.report(&self.key.clone(), &placed);
        true
    }
}

struct Placed {
    sections: Vec<NavSection>,
    select: Event,
    chosen: std::collections::HashMap<String, Event>,
    bound: std::collections::HashMap<String, Event>,
    adds: std::collections::HashMap<String, Event>,
    bound_adds: std::collections::HashMap<String, Event>,
}

/// The sections `items` stand for under `title`, in order: places outside a section run
/// together under the list's title, each section is its own under its heading.
fn place(key: &str, title: Option<String>, items: Vec<NavEntry>, select: Event) -> Placed {
    let mut chosen: std::collections::HashMap<String, Event> = std::collections::HashMap::new();
    let mut bound: std::collections::HashMap<String, Event> = std::collections::HashMap::new();
    for item in items.iter().flat_map(NavEntry::places) {
        match item.select {
            Event::None => {}
            Event::Action(_) => {
                bound.insert(item.key.clone(), item.select.clone());
            }
            Event::View(_) => {
                chosen.insert(item.key.clone(), item.select.clone());
            }
        }
    }
    let mut sections: Vec<NavSection> = Vec::new();
    let mut adds: std::collections::HashMap<String, Event> = std::collections::HashMap::new();
    let mut bound_adds: std::collections::HashMap<String, Event> = std::collections::HashMap::new();
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
                let mut built = NavSection::new(id.clone(), section.title, places);
                if section.collapsible {
                    built = built.collapsible();
                }
                if let Some(add) = section.add {
                    let icon = add.icon.unwrap_or(crate::icons::Icon::Plus);
                    built = built
                        .trailing_action(crate::widget::NavAction::new("add", icon, add.label));
                    match add.run {
                        Event::Action(_) => bound_adds.insert(id, add.run),
                        _ => adds.insert(id, add.run),
                    };
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
    Placed {
        sections,
        select,
        chosen,
        bound,
        adds,
        bound_adds,
    }
}

impl NavList {
    /// Answer choosing a place and pressing a section's add with the events `placed` carries.
    fn report(&self, key: &str, placed: &Placed) {
        self.bind_actions(&placed.bound, &placed.bound_adds);
        if !placed.adds.is_empty() {
            let mut runs: std::collections::HashMap<String, Box<dyn FnMut()>> = placed
                .adds
                .iter()
                .filter_map(|(id, event)| Some((id.clone(), event.clone().handler()?)))
                .collect();
            self.set_section_action(move |section, action| {
                if action == "add"
                    && let Some(run) = runs.get_mut(section)
                {
                    run();
                }
            });
        }
        if placed.chosen.is_empty() && placed.select.is_none() {
            return;
        }
        let mut runs: std::collections::HashMap<String, Box<dyn FnMut()>> = placed
            .chosen
            .iter()
            .filter_map(|(key, event)| Some((key.clone(), event.clone().handler()?)))
            .collect();
        let select = placed.select.clone();
        self.set_on_view(key, move |id| {
            if let Some(run) = runs.get_mut(&id) {
                run();
            }
            select.fire(Some(Value::Text(id)));
        });
    }
}

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
