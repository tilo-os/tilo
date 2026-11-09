use std::collections::BTreeMap;

use super::*;
use crate::widget::ListRowProps;

/// The shipped catalogue is the toolkit's: tools that read it see the widgets as they are.
#[test]
fn the_shipped_catalogue_is_the_one_the_toolkit_generates() {
    let shipped = tilo_ui_graph::component::SHIPPED;
    let generated = catalogue().to_json();
    eprintln!("CATALOGUE-BEGIN\n{generated}CATALOGUE-END");
    assert!(
        shipped == generated,
        "crates/dev/ui-graph/components.json is stale: a widget's description changed after it \
         was last written, and tilolang and the tools read that file as the package `ui`. Write \
         it again and commit it:\n\
         \x20   cargo run -p tilo-ui --example component-catalogue > crates/dev/ui-graph/components.json"
    );
}

/// Every described part has its doc text, and the names a description uses are its own.
#[test]
fn every_description_is_documented_and_consistent() {
    let catalogue = catalogue();
    let mut seen: Vec<&str> = catalogue
        .components
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    let all = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        all,
        "the catalogue names each component once: a hand-written entry next to the widget's \
         own description registers it twice"
    );
    for d in &catalogue.components {
        assert!(!d.doc.is_empty(), "{} has its doc text", d.name);
        let mut names: Vec<&str> = d
            .props
            .iter()
            .map(|p| p.name.as_str())
            .chain(d.slots.iter().map(|s| s.name.as_str()))
            .chain(d.events.iter().map(|e| e.name.as_str()))
            .chain(d.bindings.iter().map(|b| b.name.as_str()))
            .chain(d.state.iter().map(|s| s.name.as_str()))
            .collect();
        let all = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all, "{} names each part once", d.name);
        assert!(d.slots.iter().filter(|s| s.default).count() <= 1);
        for slot in &d.slots {
            let names = match &slot.accepts {
                Accepts::View => Vec::new(),
                Accepts::Component(name) => vec![name.clone()],
                Accepts::OneOf(names) => names.clone(),
            };
            for name in names {
                assert!(
                    catalogue.get(&name).is_some(),
                    "{}.{} accepts described components, and {name} is one",
                    d.name,
                    slot.name
                );
            }
        }
    }
}

/// An instance as a test gives it: values by prop, events queued on `queue`.
struct Given {
    key: String,
    props: BTreeMap<String, Value>,
    queue: crate::Intents<Fired>,
    events: Vec<&'static str>,
}

impl Source for Given {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, name: &str) -> Event {
        if self.events.contains(&name) {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        _slot: &str,
        _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }
}

fn given(key: &str, props: &[(&str, Value)]) -> Given {
    Given {
        key: key.to_owned(),
        props: props
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
        queue: crate::Intents::default(),
        events: Vec::new(),
    }
}

/// A value of `ty` for a required prop.
fn sample(ty: &Type) -> Value {
    match ty {
        // Text, and a case a binding holds, travel as their words.
        Type::Text | Type::Enum => Value::Text("x".into()),
        Type::Int => Value::Int(1),
        Type::Float => Value::Float(1.0),
        Type::Bool => Value::Bool(true),
        Type::Principal => Value::Principal {
            identity: "ada".into(),
            name: "Ada".into(),
        },
        Type::Resource => Value::Resource("r".into()),
        Type::Link => Value::Link {
            reference: "tl:a.b".into(),
            title: "x".into(),
        },
        Type::Time => Value::Text("2026-06-30T08:42:00Z".into()),
        Type::Icon => Value::Icon("pin".into()),
        Type::Picture => Value::Picture(tilo_ui_graph::component::Picture {
            src: "tilo-blob://dot".into(),
        }),
        Type::RichText => Value::Rich(tilo_ui_graph::rich::RichText::default()),
        Type::Choice { cases, .. } => Value::Choice(cases.first().cloned().unwrap_or_default()),
        Type::Option(_) => Value::None,
        Type::List(_) | Type::Set(_) => Value::List(Vec::new()),
        Type::Map { .. } => Value::Map(Vec::new()),
        Type::Action => Value::Action(tilo_ui_graph::component::Offered::default()),
        // The first case of the closed set, each field without a default given a sample.
        Type::Cases { name } => {
            let described = crate::component::catalogue()
                .enums
                .into_iter()
                .find(|described| &described.name == name)
                .unwrap_or_else(|| panic!("`{name}` is described"));
            let case = &described.cases[0];
            Value::Case {
                case: case.name.clone(),
                fields: case
                    .fields
                    .iter()
                    .filter(|field| field.default.is_none())
                    .map(|field| (field.name.clone(), sample(&field.ty)))
                    .collect(),
            }
        }
    }
}

#[test]
fn every_described_widget_builds_from_its_required_props() {
    for entry in registry() {
        let description = (entry.describe)();
        let props: Vec<(&str, Value)> = description
            .props
            .iter()
            .filter(|p| p.required)
            .map(|p| (p.name.as_str(), sample(&p.ty)))
            .collect();
        let mut source = given("k", &props);
        assert!(
            (entry.build)(&mut source).is_ok(),
            "{} builds",
            description.name
        );
    }
}

/// Every described widget takes its props where it stands: the adapter reaches it as its own
/// type ([`crate::described!`]) and sets them, so an instance outlives a change to it and keeps
/// what the person was doing with it — the focus in a field, a drag under a finger, the menu a
/// press opened. A widget that answers no type of its own, or refuses every change, is built
/// again each time its props are told to it.

#[test]
fn every_described_widget_takes_its_props_where_it_stands() {
    let mut no_type = Vec::new();
    let mut built_again = Vec::new();
    for entry in registry() {
        let description = (entry.describe)();
        let props: Vec<(&str, Value)> = description
            .props
            .iter()
            .filter(|p| p.required)
            .map(|p| (p.name.as_str(), sample(&p.ty)))
            .collect();
        let mut source = given("k", &props);
        let Ok(mut widget) = (entry.build)(&mut source) else {
            continue;
        };
        if widget.as_any_mut().is_none() {
            no_type.push(description.name.clone());
            continue;
        }
        let before = std::ptr::from_ref::<dyn crate::widget::Widget>(widget.as_ref());
        let mut next = given("k", &props);
        let taken = (entry.update)(&mut widget, &mut next);
        if taken != Ok(true) {
            built_again.push(description.name.clone());
            continue;
        }
        assert_eq!(
            before,
            std::ptr::from_ref::<dyn crate::widget::Widget>(widget.as_ref()),
            "{} is the instance it was",
            description.name
        );
    }
    assert!(
        no_type.is_empty(),
        "these described widgets answer no type of their own, so a change to them builds them \
         again: declare `described!();` inside their `impl Widget`:\n  {}",
        no_type.join("\n  ")
    );
    assert!(
        built_again.is_empty(),
        "these described widgets are built again when their own props are given to them, which \
         loses the person's focus, press and whatever they opened: take the props in place in \
         `set_props`:\n  {}",
        built_again.join("\n  ")
    );
}

/// Props given in place carry no built views, so a widget that shows its parts' views — a map's
/// pins, a board's cards — keeps each part's view where it stands and takes from the props only
/// what it says *around* them. It therefore takes the props in place only while they name the
/// parts it already holds, and is built again when they name others.
///
/// The alternative is what left a stale node behind after `5fab1bdd584`: a map patched the pins
/// it recognised and kept the pin of a place the arrangement had closed, so a closed window's
/// title stayed on the canvas and in the accessibility tree; and a board rebuilt its cards from
/// props that carried none of their views, so every card showed empty.
#[test]
fn a_widget_holding_parts_takes_other_parts_only_by_being_built_again() {
    use crate::widget::{
        Board, BoardCardProps, BoardLaneProps, BoardProps, Map, MapPinProps, MapProps, Text,
    };

    fn pin(key: &str, body: &str) -> MapPinProps {
        MapPinProps {
            key: key.into(),
            body: vec![Box::new(Text::new(body))],
            ..MapPinProps::default()
        }
    }

    fn map(pins: Vec<MapPinProps>) -> Map {
        <Map as Described>::from_props(
            "map",
            MapProps {
                name: Some("Canvas".into()),
                ground: false,
                pins,
            },
        )
    }

    /// The names a widget lays out for assistive technology, its parts' views included: read
    /// from the widget as it is laid out, so what its views say stands in its tree.
    fn said(root: &dyn crate::widget::Widget) -> Vec<String> {
        fn walk(w: &dyn crate::widget::Widget, out: &mut Vec<String>) {
            if let Some(node) = w.accessibility() {
                out.extend(node.name);
            }
            for child in w.children() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out
    }

    /// `widget`, laid out as the window lays it out, with the names it says once it is.
    fn laid_out<W: crate::widget::Widget + 'static>(
        widget: W,
    ) -> (crate::headless::FrameHarness, Vec<String>) {
        let host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            widget,
            (900, 600),
            crate::theme::Theme::dark(),
        );
        let said = said(host.root::<W>());
        (host, said)
    }

    let pins = |keys: &[&str]| -> Vec<MapPinProps> {
        keys.iter()
            .map(|key| pin(key, &format!("the place {key}")))
            .collect()
    };
    let props = |keys: &[&str]| MapProps {
        name: Some("Canvas".into()),
        ground: false,
        pins: pins(keys),
    };

    let (mut host, before) = laid_out(map(pins(&["a", "b"])));
    assert!(
        before.iter().any(|name| name.contains("the place b")),
        "the map shows its pins' views: {before:?}"
    );
    assert!(
        host.root_mut::<Map>().set_props(props(&["a", "b"])),
        "a map given the pins it holds takes where they stand"
    );
    let after = said(host.root::<Map>());
    assert!(
        after.iter().any(|name| name.contains("the place b")),
        "the pins' views stay: {after:?}"
    );
    for other in [&["a"][..], &["a", "b", "c"][..], &["b", "a"][..]] {
        let mut held = map(pins(&["a", "b"]));
        assert!(
            !held.set_props(props(other)),
            "a map given pins it does not hold shows another canvas, and is built again: \
             {other:?}"
        );
    }

    let card = |key: &str| BoardCardProps {
        key: key.into(),
        body: vec![Box::new(Text::new(&format!("the card {key}")))],
        ..BoardCardProps::default()
    };
    let board = |cards: &[&str]| BoardProps {
        name: Some("Plan".into()),
        lanes: vec![BoardLaneProps {
            key: "todo".into(),
            title: Some("To do".into()),
            cards: cards.iter().map(|key| card(key)).collect(),
            ..BoardLaneProps::default()
        }],
    };
    let (mut host, before) = laid_out(<Board as Described>::from_props(
        "board",
        board(&["a", "b"]),
    ));
    assert!(
        before.iter().any(|name| name.contains("the card b")),
        "the board shows its cards' views: {before:?}"
    );
    assert!(
        host.root_mut::<Board>().set_props(board(&["a", "b"])),
        "a board given the lanes and cards it holds takes where it stands"
    );
    let after = said(host.root::<Board>());
    assert!(
        after.iter().any(|name| name.contains("the card b")),
        "the cards' views stay: {after:?}"
    );
    for other in [&["a"][..], &["a", "b", "c"][..], &["b", "a"][..]] {
        let mut held = <Board as Described>::from_props("board", board(&["a", "b"]));
        assert!(
            !held.set_props(board(other)),
            "a board whose cards changed shows another board, and is built again: {other:?}"
        );
    }
}

#[test]
fn a_missing_required_prop_or_a_value_of_another_type_is_refused() {
    let mut nothing = given("k", &[]);
    let error = build("EmptyState", &mut nothing).err();
    assert_eq!(error, Some(Error::missing("EmptyState", "title")));
    let mut wrong = given("k", &[("title", Value::Int(3))]);
    assert!(build("EmptyState", &mut wrong).is_err());
    let mut unknown = given("k", &[]);
    assert!(build("Gauge", &mut unknown).is_err());
}

#[test]
fn props_read_the_values_an_instance_gives() {
    let mut source = given(
        "row",
        &[
            ("title", Value::Text("Q3 plan".into())),
            ("secondary", Value::Text("Ship it".into())),
            ("mark", Value::Icon("pin".into())),
            ("selected", Value::Bool(true)),
        ],
    );
    source.events.push("activate");
    let props = ListRowProps::from_source(&mut source).unwrap_or_default();
    assert_eq!(props.key, "row");
    assert_eq!(props.title, "Q3 plan");
    assert_eq!(props.secondary.as_deref(), Some("Ship it"));
    assert_eq!(props.mark, Some(crate::icons::Icon::Pin));
    assert!(props.selected);
    assert!(!props.activate.is_none(), "the event is the host's intent");
    props.activate.fire(None);
    assert_eq!(
        source.queue.take(),
        vec![Fired {
            key: "row".into(),
            event: "activate".into(),
            value: None
        }]
    );
}

#[test]
fn a_changed_text_is_taken_in_place_and_another_role_is_built_again() {
    let mut first = given("t", &[("value", Value::Text("1".into()))]);
    let mut text =
        build("Text", &mut first).unwrap_or_else(|_| Box::new(crate::widget::Spacer::fixed(0.0)));
    let mut next = given("t", &[("value", Value::Text("2".into()))]);
    assert_eq!(update("Text", &mut text, &mut next), Ok(true));
    let mut titled = given(
        "t",
        &[
            ("value", Value::Text("2".into())),
            ("role", Value::Choice("title".into())),
        ],
    );
    assert_eq!(update("Text", &mut text, &mut titled), Ok(false));
    assert_eq!(text.test_id(), Some("t"));
}

/// An item of a list as a host renders it: a row titled `title` showing `resource`,
/// with an avatar when `avatar`, its menu one destructive command.
struct Item {
    key: String,
    title: String,
    resource: String,
    avatar: bool,
}

/// An instance of an item, its parts read as components.
struct ItemSource<'a> {
    item: &'a Item,
    queue: crate::Intents<Fired>,
}

impl Source for ItemSource<'_> {
    fn key(&self) -> &str {
        &self.item.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        match name {
            "item" => Some(Value::Resource(self.item.resource.clone())),
            _ => None,
        }
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn slot_kinds(&mut self, slot: &str) -> Vec<Option<String>> {
        match slot {
            "row" => vec![Some("ListRow".to_owned())],
            "menu" => vec![Some("MenuItem".to_owned())],
            _ => Vec::new(),
        }
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        match slot {
            "row" => {
                let row = given(
                    &self.item.key,
                    &[("title", Value::Text(self.item.title.clone()))],
                );
                read(&mut AvatarRow {
                    row,
                    avatar: self.item.avatar,
                })?;
                Ok(1)
            }
            "menu" => {
                let mut command = given("delete", &[("label", Value::Text("Delete".into()))]);
                command
                    .props
                    .insert("destructive".into(), Value::Bool(true));
                command.queue = self.queue.clone();
                command.events.push("choose");
                read(&mut command)?;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

/// A row whose avatar slot holds one when asked.
struct AvatarRow {
    row: Given,
    avatar: bool,
}

impl Source for AvatarRow {
    fn key(&self) -> &str {
        self.row.key()
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.row.prop(name)
    }

    fn event(&mut self, name: &str) -> Event {
        self.row.event(name)
    }

    fn binding(&mut self, name: &str) -> Option<BoundValue> {
        self.row.binding(name)
    }

    fn children(&mut self, slot: &str) -> Vec<Element> {
        self.row.children(slot)
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        if slot == "avatar" && self.avatar {
            read(&mut given("who", &[("name", Value::Text("Ada".into()))]))?;
            return Ok(1);
        }
        Ok(0)
    }
}

/// Rows a test renders: every one at hand, none rendered until asked.
struct Rendered {
    items: Vec<Item>,
    asked: Vec<std::ops::Range<usize>>,
    queue: crate::Intents<Fired>,
    /// Rows the host renders anew, reported at the list's next poll.
    changed: Vec<String>,
}

impl RowSource for Rendered {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.items.get(index).map(|item| item.key.clone())
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.items.iter().position(|item| item.key == key)
    }

    fn request_range(&mut self, range: std::ops::Range<usize>) {
        self.asked.push(range);
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(item) = self.items.iter().find(|item| item.key == key) else {
            return Ok(false);
        };
        read(&mut ItemSource {
            item,
            queue: self.queue.clone(),
        })?;
        Ok(true)
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        self.items
            .iter()
            .find(|item| item.resource == id)
            .map(|item| item.key.clone())
    }

    fn rendered(&self, key: &str) -> bool {
        self.items.iter().any(|item| item.key == key)
    }

    fn take_changes(&mut self) -> Vec<crate::widget::virtual_list::ListModelChange<String>> {
        if self.changed.is_empty() {
            return Vec::new();
        }
        vec![crate::widget::virtual_list::ListModelChange::RowsChanged {
            keys: std::mem::take(&mut self.changed),
        }]
    }

    fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }
}

/// A list's instance whose rows a host renders.
struct ListSource {
    rows: SharedRows,
    queue: crate::Intents<Fired>,
}

impl Source for ListSource {
    fn key(&self) -> &str {
        "list"
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        match name {
            "selection_follows_focus" | "in_column" => Some(Value::Bool(true)),
            _ => None,
        }
    }

    fn event(&mut self, name: &str) -> Event {
        Event::View(Intent::new(&self.queue, "list", name))
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        _slot: &str,
        _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }

    fn rows(&mut self, _slot: &str) -> Option<SharedRows> {
        Some(std::rc::Rc::clone(&self.rows))
    }
}

/// A list whose items a test gives whole, each a `ui.ListItem` of the props it names.
struct WholeList {
    props: BTreeMap<String, Value>,
    items: Vec<Given>,
}

impl Source for WholeList {
    fn key(&self) -> &str {
        "list"
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        if slot != "rows" {
            return Ok(0);
        }
        for item in &mut self.items {
            read(item)?;
        }
        Ok(self.items.len())
    }
}

/// The list `title` heads the rows that name no group with, over the items of `items`, each a
/// key and the group it names.
fn grouped_list(title: Option<&str>, items: &[(&str, &str)]) -> crate::widget::DescribedList {
    let mut props = BTreeMap::new();
    props.insert("in_column".to_owned(), Value::Bool(true));
    if let Some(title) = title {
        props.insert("ungrouped_title".to_owned(), Value::Text(title.into()));
    }
    let items = items
        .iter()
        .map(|(key, group)| given(key, &[("group", Value::Text((*group).into()))]))
        .collect();
    let mut source = WholeList { props, items };
    let props = crate::widget::VirtualListProps::from_source(&mut source).expect("reads");
    <crate::widget::DescribedList as Described>::from_props("list", props)
}

fn rendered_list(items: Vec<Item>) -> (crate::widget::DescribedList, crate::Intents<Fired>) {
    let (list, queue, _) = rendered_list_and_rows(items);
    (list, queue)
}

/// [`rendered_list`], with the rows its host renders, to change.
fn rendered_list_and_rows(
    items: Vec<Item>,
) -> (
    crate::widget::DescribedList,
    crate::Intents<Fired>,
    std::rc::Rc<std::cell::RefCell<Rendered>>,
) {
    let queue = crate::Intents::default();
    let rendered = std::rc::Rc::new(std::cell::RefCell::new(Rendered {
        items,
        asked: Vec::new(),
        queue: queue.clone(),
        changed: Vec::new(),
    }));
    let rows: SharedRows = rendered.clone();
    let mut source = ListSource {
        rows,
        queue: queue.clone(),
    };
    let props = crate::widget::VirtualListProps::from_source(&mut source).expect("reads");
    (
        <crate::widget::DescribedList as Described>::from_props("list", props),
        queue,
        rendered,
    )
}

fn item(key: &str, title: &str, avatar: bool) -> Item {
    Item {
        key: key.into(),
        title: title.into(),
        resource: format!("doc/notes:{key}"),
        avatar,
    }
}

#[test]
fn a_list_of_rendered_rows_reads_them_from_its_host_and_its_menu_from_the_row() {
    let (mut list, queue) = rendered_list(vec![
        item("a", "Q3 plan", false),
        item("b", "Groceries", false),
    ]);
    let virtual_list = list.list_mut().expect("the list");
    assert_eq!(crate::widget::ListModel::len(virtual_list.model()), 2);
    assert_eq!(
        crate::widget::ListModel::label(virtual_list.model(), 1).as_deref(),
        Some("Groceries"),
        "a row is named by its title, for type-ahead"
    );
    assert!(virtual_list.reveal(&"b".to_owned()));
    let area = list.menu_area_mut().expect("the rows share one menu");
    assert!(area.open_at(vello::kurbo::Point::ZERO, false));
    let labels: Vec<Option<&str>> = area
        .menu()
        .items()
        .iter()
        .map(crate::widget::MenuItem::label)
        .collect();
    assert_eq!(labels, vec![Some("Delete")]);
    // Opening the menu selects its row, reported with the resource it shows.
    assert!(queue.take().iter().any(|fired| fired.event == "select"
        && fired.value == Some(Value::Resource("doc/notes:b".into()))));
    area.choose(0);
    assert!(
        queue
            .take()
            .iter()
            .any(|fired| fired.key == "delete" && fired.event == "choose"),
        "choosing it fires the command's event"
    );
    // The list's selection is the row of the resource shown beside it.
    assert!(list.select_presenting(Some("doc/notes:a")));
    let selected: Vec<String> = list
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    assert_eq!(selected, ["a".to_owned()]);
    assert!(!list.select_presenting(Some("doc/notes:a")), "already so");
}

/// A row its host renders anew (an item's title as it is typed) is mounted again where it
/// stood: the described list says what its list says, and the window keeps its tree.
#[test]
fn a_rendered_row_that_changes_keeps_the_windows_tree() {
    let (list, _queue, rows) = rendered_list_and_rows(vec![
        item("a", "Q3 plan", false),
        item("b", "Groceries", false),
    ]);
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        list,
        (400, 300),
        crate::theme::Theme::dark(),
    );
    harness.relayout();
    let (rebuilt, nodes) = (harness.rebuilt_frames(), harness.layout_node_ids());

    for title in ["Q3 plan!", "Q3 plan!!", "Q3 plan!!!"] {
        {
            let mut rows = rows.borrow_mut();
            rows.items[0].title = title.into();
            rows.changed.push("a".into());
        }
        harness.relayout();
        assert!(
            harness.last_relayout().remounted,
            "the row is mounted in place"
        );
    }
    assert_eq!(
        harness.rebuilt_frames() - rebuilt,
        0,
        "three changed rows rebuilt nothing"
    );
    assert_eq!(
        harness.layout_node_ids().len(),
        nodes.len(),
        "the window's tree keeps its shape"
    );
    let shown = harness.inspection_json().expect("inspects");
    assert!(
        shown.contains("Q3 plan!!!"),
        "the row shows what it says now"
    );
}

/// A conversation read from its first row: its rows stand at the width a page's blocks are
/// read at, under the top of the column, rather than across a wide window or under a band the
/// window's foot left.
#[test]
fn a_page_column_holds_the_readable_measure_from_its_first_row() {
    fn pages<'a>(
        node: &'a crate::headless::InspectionNode,
        out: &mut Vec<&'a crate::headless::InspectionNode>,
    ) {
        if node.widget == "Page" {
            out.push(node);
        }
        for child in &node.children {
            pages(child, out);
        }
    }

    let mut source = WholeList {
        props: BTreeMap::from([
            ("page_column".to_owned(), Value::Bool(true)),
            ("at_measure".to_owned(), Value::Bool(true)),
        ]),
        items: ["a", "b", "c"]
            .iter()
            .map(|key| given(key, &[("title", Value::Text((*key).into()))]))
            .collect(),
    };
    let props = crate::widget::VirtualListProps::from_source(&mut source).expect("reads");
    let list = <crate::widget::DescribedList as Described>::from_props("list", props);
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        list,
        (1600, 900),
        crate::theme::Theme::dark(),
    );
    harness.relayout();
    let inspection = harness.inspection();
    let mut found = Vec::new();
    pages(&inspection.root, &mut found);
    assert_eq!(found.len(), 3, "one page row per message");
    let measure = f64::from(crate::widget::arrange::MEASURE_REM)
        * f64::from(crate::widget::arrange::rem_of(&crate::theme::Theme::dark()));
    for page in &found {
        let width = page.children.first().expect("a row").rect[2];
        assert!(
            (width - measure).abs() <= 1.0,
            "a row at the readable measure: {width}"
        );
    }
    let first = found
        .iter()
        .map(|row| row.rect[1])
        .fold(f64::INFINITY, f64::min);
    assert!(
        first < 100.0,
        "the first message under the top of the column: {first}"
    );
}

#[test]
fn rows_that_name_no_group_stand_under_their_own_heading() {
    use crate::widget::RowGroup;
    let list = grouped_list(
        Some("No date"),
        &[("a", "Jul 1"), ("b", ""), ("c", "Jul 1")],
    );
    let model = list.list().expect("the list").model();
    assert_eq!(
        crate::widget::ListModel::group_at(model, 0),
        Some(RowGroup::heading("Jul 1"))
    );
    assert_eq!(
        crate::widget::ListModel::group_at(model, 1),
        Some(RowGroup::heading("No date")),
        "a row whose item names no group is not the last group's"
    );
    // A list given no heading for them leaves those rows among the others, as before.
    let list = grouped_list(None, &[("a", "Jul 1"), ("b", "")]);
    let model = list.list().expect("the list").model();
    assert_eq!(crate::widget::ListModel::group_at(model, 1), None);
    // A row standing in for an item removed unseen stays out of every fold, headed or not.
    let mut props = BTreeMap::new();
    props.insert("ungrouped_title".to_owned(), Value::Text("No date".into()));
    let mut source = WholeList {
        props,
        items: vec![given(
            "a",
            &[
                ("group", Value::Text("Jul 1".into())),
                ("removed", Value::Bool(true)),
            ],
        )],
    };
    let props = crate::widget::VirtualListProps::from_source(&mut source).expect("reads");
    let list = <crate::widget::DescribedList as Described>::from_props("list", props);
    assert_eq!(
        crate::widget::ListModel::group_at(list.list().expect("the list").model(), 0),
        None
    );
}

#[test]
fn an_icon_only_button_is_a_plain_glyph_unless_given_a_role() {
    let mut source = given(
        "pin",
        &[
            ("label", Value::Text("Pin".into())),
            ("icon", Value::Icon("pin".into())),
            ("icon_only", Value::Bool(true)),
            ("toggled", Value::Bool(true)),
        ],
    );
    let props = crate::widget::ButtonProps::from_source(&mut source).expect("reads");
    let button = <crate::widget::Button as Described>::from_props("pin", props);
    assert_eq!(button.role_of(), crate::widget::ButtonRole::Plain);
    assert!(button.is_toggled());
}
