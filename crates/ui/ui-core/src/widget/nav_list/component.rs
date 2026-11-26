//! [`NavList`] and its items as described components.

use std::collections::HashMap;

use crate::widget::attached::MenuPart;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::OfferedAction,
    component::Person, component::ResourceId, component::Value,
};

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
    #[component(part(default))]
    pub items: Vec<NavEntry>,
    /// A place was chosen: its key is the value.
    #[component(event(value = String))]
    pub select: Event,
    /// A place was dropped on another: carries the place's key and the key it was dropped on,
    /// in that order. The app moves its own list; the list keeps no order of its own.
    #[component(event(value = Vec<String>))]
    pub reorder: Event,
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
    /// The resource the group is, when it is one (a category of channels): the group is kept by
    /// it rather than by where it stands, so a fold the person made stays with the group when it
    /// is renamed or moved among the others.
    pub item: Option<ResourceId>,
    /// The action that makes another place in the group ("New project"), drawn at the end of
    /// the heading.
    pub add: Option<OfferedAction>,
    /// The group's own menu (a right click on its heading, the Menu key, a long press): what
    /// is done to the group itself (renaming it, moving it, removing it), as the places under
    /// it carry theirs. A group whose view places none offers none.
    #[component(part)]
    pub menu: Vec<MenuPart>,
    /// The places, in order.
    #[component(part(default))]
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
    /// A quieter line under the name: what the place holds as it stands (the line half-written
    /// in its draft, where a conversation last left off).
    pub secondary: Option<String>,
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
    /// The person the place shows is talking now (in a call): their face rings as they are heard.
    pub speaking: bool,
    /// The place this one stands under, by its key: the rows of a list that nests its places
    /// (an outline of headings) fold under the one they name, a level in, and stand open. A key
    /// that names no place of the list leaves the place at its own level, as a row whose parent
    /// is gone does.
    pub parent: Option<String>,
    /// The place's own menu (a right click, the Menu key, a long press): what is done to the
    /// place itself, in the order the view writes it. A place whose view places none offers
    /// none — unless the host gives the list a menu of its own, which then answers every row.
    #[component(part)]
    pub menu: Vec<MenuPart>,
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
        if let Some(secondary) = props.secondary {
            item = item.secondary(secondary);
        }
        if let Some(person) = props.person {
            item = item.person(person.identity, None, props.speaking);
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

/// The places `items` stand for, folded: a place that names another of the list as its parent
/// stands under it, a level in and open, and every other at its own level. A place whose parent
/// names no place of the list — or names a place of a cycle — stands at its own level too, so no
/// place is lost.
fn nested(items: Vec<NavItemProps>) -> Vec<NavItem> {
    let known: std::collections::HashSet<String> =
        items.iter().map(|item| item.key.clone()).collect();
    let mut children: std::collections::HashMap<String, Vec<NavItemProps>> =
        std::collections::HashMap::new();
    let mut roots: Vec<NavItemProps> = Vec::new();
    let mut order: Vec<NavItemProps> = Vec::with_capacity(items.len());
    for item in items {
        order.push(item.clone());
        match item
            .parent
            .clone()
            .filter(|parent| parent != &item.key && known.contains(parent))
        {
            Some(parent) => children.entry(parent).or_default().push(item),
            None => roots.push(item),
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = build(roots, &mut children, &mut seen);
    // A place no branch reached — one caught in a cycle of parents — stands at the top.
    for item in order {
        if seen.contains(&item.key) {
            continue;
        }
        seen.insert(item.key.clone());
        out.push(NavItem::from(item));
    }
    out
}

/// The places `items` as rows, each with the places that stand under it, in order.
fn build(
    items: Vec<NavItemProps>,
    children: &mut std::collections::HashMap<String, Vec<NavItemProps>>,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<NavItem> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if !seen.insert(item.key.clone()) {
            continue;
        }
        let key = item.key.clone();
        let mut built = NavItem::from(item);
        if let Some(under) = children.remove(&key) {
            let kids = build(under, children, seen);
            if !kids.is_empty() {
                built = built.children(kids).expanded();
            }
        }
        out.push(built);
    }
    out
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
            reorder,
        } = props;
        let placed = place(key, title, items, select);
        let mut list = Self::new(placed.sections.clone()).with_id(key);
        list.set_reorder_event(reorder);
        list.set_row_menus(&placed.menus);
        if let Some(selected) = selected {
            list = list.selected(&selected);
        }
        if rail {
            list = list.rail();
        }
        list.report(&placed);
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
            reorder,
        } = props;
        let placed = place(&self.key, title, items, select);
        self.set_reorder_event(reorder);
        self.set_row_menus(&placed.menus);
        self.set_sections(placed.sections.clone());
        self.set_selected(selected.as_deref());
        self.set_rail_rows(rail);
        self.report(&placed);
        true
    }
}

struct Placed {
    sections: Vec<NavSection>,
    /// Each row's own menu, by row key (`item:<key>`, `section:<id>`).
    menus: HashMap<String, Vec<MenuPart>>,
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
    let mut menus: HashMap<String, Vec<MenuPart>> = HashMap::new();
    for item in items.iter().flat_map(NavEntry::places) {
        if !item.menu.is_empty() {
            menus.insert(
                format!("item:{}", item.key),
                item.menu.iter().map(MenuPart::again).collect(),
            );
        }
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
    let mut loose: Vec<NavItemProps> = Vec::new();
    let mut title = title;
    let mut flush = |loose: &mut Vec<NavItemProps>, sections: &mut Vec<NavSection>| {
        if loose.is_empty() {
            return;
        }
        let id = format!("{key}/{}", sections.len());
        let items = nested(std::mem::take(loose));
        sections.push(match title.take() {
            Some(title) => NavSection::new(id, title, items),
            None => NavSection::untitled(id, items),
        });
    };
    for entry in items {
        match entry {
            NavEntry::Item(item) => loose.push(item),
            NavEntry::Section(section) => {
                flush(&mut loose, &mut sections);
                let id = match &section.item {
                    Some(item) => format!("{key}/item:{}", item.0),
                    None => format!("{key}/{}", sections.len()),
                };
                if !section.menu.is_empty() {
                    menus.insert(
                        format!("section:{id}"),
                        section.menu.iter().map(MenuPart::again).collect(),
                    );
                }
                let places = nested(section.items);
                let mut built = NavSection::new(id.clone(), section.title, places);
                if section.collapsible {
                    built = built.collapsible();
                }
                if let Some(add) = section.add {
                    let icon = add.icon.unwrap_or(tilo_ui_base::icons::Icon::Plus);
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
        menus,
        select,
        chosen,
        bound,
        adds,
        bound_adds,
    }
}

impl NavList {
    /// Answer choosing a place and pressing a section's add with the events `placed` carries.
    fn report(&self, placed: &Placed) {
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
        self.set_on_choose(move |id| {
            if let Some(run) = runs.get_mut(&id) {
                run();
            }
            select.fire(Some(Value::Text(id)));
        });
    }
}

const NAV_PARTS: [&str; 2] = ["NavItem", "NavSection"];

/// A navigation list's places and sections, in order.
impl tilo_ui_base::component::PartItem for NavEntry {
    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::OneOf(
            NAV_PARTS.iter().map(|name| (*name).to_owned()).collect(),
        )
    }

    fn read_all(
        source: &mut dyn tilo_ui_base::component::Source,
        part: &str,
    ) -> Result<Vec<Self>, String> {
        let kinds = source.part_kinds(part);
        let mut read = Vec::with_capacity(kinds.len());
        let mut index = 0usize;
        source.components(part, &mut |child| {
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
        Ok(read)
    }
}

impl tilo_ui_base::component::PartItem for NavItemProps {
    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(NavItemProps::NAME.into())
    }

    fn read_all(
        source: &mut dyn tilo_ui_base::component::Source,
        part: &str,
    ) -> Result<Vec<Self>, String> {
        let mut read = Vec::new();
        source.components(part, &mut |child| {
            read.push(NavItemProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        Ok(read)
    }
}
