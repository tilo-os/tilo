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
        "crates/dev/ui-graph/components.json is stale; write it again with \
         `cargo run -p tilo-ui --example component-catalogue > crates/dev/ui-graph/components.json`"
    );
}

/// Every described part has its doc text, and the names a description uses are its own.
#[test]
fn every_description_is_documented_and_consistent() {
    let catalogue = catalogue();
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
        Type::Text => Value::Text("x".into()),
        Type::Int => Value::Int(1),
        Type::Float => Value::Float(1.0),
        Type::Bool => Value::Bool(true),
        Type::Principal => Value::Principal {
            identity: "ada".into(),
            name: "Ada".into(),
        },
        Type::Resource => Value::Resource("r".into()),
        Type::Time => Value::Text("2026-06-30T08:42:00Z".into()),
        Type::Icon => Value::Icon("pin".into()),
        Type::RichText => Value::Rich(tilo_ui_graph::rich::RichText::default()),
        Type::Choice { cases, .. } => Value::Choice(cases.first().cloned().unwrap_or_default()),
        Type::Option(_) => Value::None,
        Type::List(_) | Type::Set(_) => Value::List(Vec::new()),
        Type::Map { .. } => Value::Map(Vec::new()),
        Type::Action => Value::Action(tilo_ui_graph::component::Offered::default()),
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
                let mut row = given(
                    &self.item.key,
                    &[("title", Value::Text(self.item.title.clone()))],
                );
                if self.item.avatar {
                    row.props.insert("avatar_space".into(), Value::Bool(false));
                }
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
