use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::component::Component;
use tilo_ui_app::component_catalogue::TOOLKIT;
use tilo_ui_app::component_testing::{Node, given, given_with_views, sample};
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::ListRowProps;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

/// The toolkit's own described widgets, as a window that builds only them composes them.
fn toolkit() -> Registry {
    Registry::of(&[&TOOLKIT, &tilo_ui_charts::COMPONENTS])
}

tilo_test_env::scenario! {
    fn every_described_widget_is_documented_builds_and_keeps_its_children;

    phase every_description_is_documented_and_consistent;
    {
        let catalogue = toolkit().catalogue();
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
                .chain(d.parts.iter().map(|s| s.name.as_str()))
                .chain(d.events.iter().map(|e| e.name.as_str()))
                .chain(d.bindings.iter().map(|b| b.name.as_str()))
                .chain(d.state.iter().map(|s| s.name.as_str()))
                .collect();
            let all = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), all, "{} names each part once", d.name);
            assert!(d.parts.iter().filter(|s| s.default).count() <= 1);
            for part in &d.parts {
                let names = match &part.accepts {
                    Accepts::View => Vec::new(),
                    Accepts::Component(name) => vec![name.clone()],
                    Accepts::OneOf(names) => names.clone(),
                };
                for name in names {
                    assert!(
                        catalogue.get(&name).is_some(),
                        "{}.{} accepts described components, and {name} is one",
                        d.name,
                        part.name
                    );
                }
            }
        }
    }

    phase every_described_widget_builds_from_its_required_props;
    {
        for entry in toolkit().entries() {
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

    phase every_described_widget_lists_the_children_it_hands_out;
    {
        use tilo_ui_base::widget::Widget;

        type Child = (*const (), String);

        fn child(widget: &dyn Widget) -> Child {
            (
                std::ptr::from_ref(widget).cast::<()>(),
                widget.name().to_owned(),
            )
        }

        fn children(widget: &mut dyn Widget) -> (usize, Option<String>) {
            let listed: Vec<Child> = widget.children().into_iter().map(child).collect();
            let mut walked = Vec::new();
            widget.for_each_child(&mut |child_widget| walked.push(child(child_widget)));
            let handed: Vec<Child> = widget
                .children_mut()
                .into_iter()
                .map(|child_widget| child(child_widget))
                .collect();
            let read = |children: &[Child]| {
                children
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let apart = if listed != handed {
                Some(format!(
                    "children lists {} ({}), children_mut hands out {} ({})",
                    listed.len(),
                    read(&listed),
                    handed.len(),
                    read(&handed)
                ))
            } else if listed != walked {
                Some(format!(
                    "children lists {} ({}), for_each_child visits {} ({})",
                    listed.len(),
                    read(&listed),
                    walked.len(),
                    read(&walked)
                ))
            } else {
                None
            };
            (listed.len(), apart)
        }

        let mut disagreeing = Vec::new();
        let mut holding: Vec<String> = Vec::new();
        for entry in toolkit().entries() {
            let description = (entry.describe)();
            // Twice: with its required props only, and with every prop it takes: a widget shows
            // parts for props a person switched on, and its lists must agree whatever it shows.
            for every_prop in [false, true] {
                let props: Vec<(&str, Value)> = description
                    .props
                    .iter()
                    .filter(|p| every_prop || p.required)
                    .map(|p| (p.name.as_str(), sample(&p.ty)))
                    .collect();
                let mut source = given_with_views("k", &props, 2);
                let Ok(mut widget) = (entry.build)(&mut source) else {
                    continue;
                };
                let (count, apart) = children(widget.as_mut());
                if count != 0 && !holding.contains(&description.name) {
                    holding.push(description.name.clone());
                }
                if let Some(apart) = apart {
                    disagreeing.push(format!("{}: {apart}", description.name));
                }
            }
        }
        assert!(
            !holding.is_empty(),
            "no described widget held a child, so this check proves nothing: give the harness views \
             in the parts the widgets ask for (`given_with_views`)"
        );
        assert!(
            disagreeing.is_empty(),
            "these widgets list one set of children read-only, walk another and hand out a third, so \
             a route by index reaches a different widget than the walk found:\n  {}",
            disagreeing.join("\n  ")
        );
    }

    phase every_described_widget_takes_its_props_where_it_stands;
    {
        let mut no_type = Vec::new();
        let mut built_again = Vec::new();
        for entry in toolkit().entries() {
            let description = (entry.describe)();
            // A board's cards are their views, and props given in place carry none of them, so
            // it is built again from the view it holds rather than left with empty cards (the
            // scenario above shows that). Being rebuilt is its intended update.
            if description.name == "Board" {
                continue;
            }
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
            let before = std::ptr::from_ref::<dyn tilo_ui_base::widget::Widget>(widget.as_ref());
            let mut next = given("k", &props);
            let taken = (entry.update)(&mut widget, &mut next);
            if taken != Ok(true) {
                built_again.push(description.name.clone());
                continue;
            }
            assert_eq!(
                before,
                std::ptr::from_ref::<dyn tilo_ui_base::widget::Widget>(widget.as_ref()),
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
}

tilo_test_env::scenario! {
    fn a_described_instance_reads_builds_and_updates_from_its_props;

    // Props given in place carry no built views, so a widget that shows its parts' views — a map's
    // pins, a board's cards — keeps each part's view where it is and takes from the props only
    // what it says *around* them. It therefore takes the props in place only while they name the
    // parts it already has, and is built again when they name others. A board, whose cards are
    // their views, is built again for any props.
    //
    // The alternative is what left a stale node behind after `5fab1bdd584`: a map patched the pins
    // it recognised and kept the pin of a place the layout had closed, so a closed window's
    // title stayed on the canvas and in the accessibility tree; and a board rebuilt its cards from
    // props that carried none of their views, so every card showed empty.
    phase a_widget_holding_parts_takes_other_parts_only_by_being_built_again;
    {
        use tilo_ui_app::widget::{
            Board, BoardCardProps, BoardListProps, BoardProps, Map, MapPinProps, MapProps, Text,
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
                    ..MapProps::default()
                },
            )
        }

        fn said(root: &dyn tilo_ui_base::widget::Widget) -> Vec<String> {
            fn walk(w: &dyn tilo_ui_base::widget::Widget, out: &mut Vec<String>) {
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

        fn laid_out<W: tilo_ui_base::widget::Widget + 'static>(
            widget: W,
        ) -> (tilo_ui_app::headless::Driver, Vec<String>) {
            let host = tilo_ui_app::headless::Driver::for_root(
                tilo_ui_app::State::new(),
                widget,
                (900, 600),
                tilo_ui_base::theme::Theme::dark(),
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
            ..MapProps::default()
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
            lists: vec![BoardListProps {
                key: "todo".into(),
                title: Some("To do".into()),
                cards: cards.iter().map(|key| card(key)).collect(),
                ..BoardListProps::default()
            }],
        };
        let (mut frame, before) = laid_out(<Board as Described>::from_props(
            "board",
            board(&["a", "b"]),
        ));
        assert!(
            before.iter().any(|name| name.contains("the card b")),
            "the board shows its cards' views: {before:?}"
        );
        // As a window gives props in place: its cards named, none of their views built.
        let mut in_place = board(&["a", "b"]);
        for list in &mut in_place.lists {
            for card in &mut list.cards {
                card.body.clear();
            }
        }
        let took = frame.root_mut::<Board>().set_props(in_place);
        let after = said(frame.root::<Board>());
        assert!(
            after.iter().any(|name| name.contains("the card b")),
            "a board given props in place shows its cards' views still (taken in place: {took}): \
             {after:?}"
        );
        for other in [&["a"][..], &["a", "b", "c"][..], &["b", "a"][..]] {
            let mut held = <Board as Described>::from_props("board", board(&["a", "b"]));
            assert!(
                !held.set_props(board(other)),
                "a board whose cards changed shows another board, and is built again: {other:?}"
            );
        }
    }

    phase a_missing_required_prop_or_a_value_of_another_type_is_refused;
    {
        let mut nothing = given("k", &[]);
        let error = toolkit().build("EmptyState", &mut nothing).err();
        assert_eq!(error, Some(tilo_ui_base::component::Error::missing("EmptyState", "title")));
        let mut wrong = given("k", &[("title", Value::Int(3))]);
        assert!(toolkit().build("EmptyState", &mut wrong).is_err());
        let mut unknown = given("k", &[]);
        assert!(toolkit().build("Gauge", &mut unknown).is_err());
    }

    phase props_read_the_values_an_instance_gives;
    {
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
        assert_eq!(props.mark, Some(tilo_ui_base::icons::Icon::Pin));
        assert!(props.selected);
        assert!(!props.activate.is_none(), "the event is the host's intent");
        props.activate.fire(None);
        assert_eq!(
            source.queue.take(),
            vec![Fired {
                key: "row".into(),
                event: "activate".into(),
                value: None,
                gesture: None,
            }]
        );
    }

    phase a_changed_text_is_taken_in_place_and_another_role_is_built_again;
    {
        let mut first = given("t", &[("value", Value::Text("1".into()))]);
        let mut text =
            toolkit().build("Text", &mut first).unwrap_or_else(|_| Box::new(tilo_ui_base::widget::Spacer::fixed(0.0)));
        let mut next = given("t", &[("value", Value::Text("2".into()))]);
        assert_eq!(toolkit().update("Text", &mut text, &mut next), Ok(true));
        let mut titled = given(
            "t",
            &[
                ("value", Value::Text("2".into())),
                ("role", Value::Choice("title".into())),
            ],
        );
        assert_eq!(toolkit().update("Text", &mut text, &mut titled), Ok(false));
        assert_eq!(text.test_id(), Some("t"));
    }

    phase an_icon_only_button_is_a_plain_glyph_unless_given_a_role;
    {
        let mut source = given(
            "pin",
            &[
                ("label", Value::Text("Pin".into())),
                ("icon", Value::Icon("pin".into())),
                ("icon_only", Value::Bool(true)),
                ("toggled", Value::Bool(true)),
            ],
        );
        let props = tilo_ui_app::widget::ButtonProps::from_source(&mut source).expect("reads");
        let button = <tilo_ui_app::widget::Button as Described>::from_props("pin", props);
        assert_eq!(button.role_of(), tilo_ui_app::widget::ButtonRole::Plain);
        assert!(button.is_toggled());
    }
}

struct Item {
    key: String,
    title: String,
    resource: String,
    avatar: bool,
}

/// A rendered row: the item it shows, its row and the menu that deletes it.
fn row_node(item: &Item, queue: &tilo_ui_base::Intents<Fired>) -> Node {
    let mut row = Node {
        kind: "ListRow",
        ..given(&item.key, &[("title", Value::Text(item.title.clone()))])
    };
    if item.avatar {
        row = row.holding(
            "avatar",
            given("who", &[("name", Value::Text("Ada".into()))]),
        );
    }
    let delete = Node::new("MenuItem", "delete", queue)
        .prop("label", Value::Text("Delete".into()))
        .prop("destructive", Value::Bool(true))
        .on("choose");
    Node {
        restores: false,
        ..Node::new("", &item.key, queue)
    }
    .prop("item", Value::Resource(item.resource.clone()))
    .holding("row", row)
    .holding("menu", delete)
}

/// Rows a test renders: every one at hand, none rendered until asked.
struct Rendered {
    items: Vec<Item>,
    asked: Vec<std::ops::Range<usize>>,
    queue: tilo_ui_base::Intents<Fired>,
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

    fn request_range(&mut self, range: std::ops::Range<usize>, _open: Option<String>) {
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
        read(&mut row_node(item, &self.queue))?;
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

    fn take_changes(
        &mut self,
    ) -> Vec<tilo_ui_base::collection_transition::ListModelChange<String>> {
        if self.changed.is_empty() {
            return Vec::new();
        }
        vec![
            tilo_ui_base::collection_transition::ListModelChange::RowsChanged {
                keys: std::mem::take(&mut self.changed),
            },
        ]
    }

    fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }
}

/// A list holding `items` in its rows part.
fn whole_list(props: &[(&str, Value)], items: Vec<Node>) -> Node {
    items.into_iter().fold(given("list", props), |list, item| {
        list.holding("rows", item)
    })
}

fn grouped_list(title: Option<&str>, items: &[(&str, &str)]) -> tilo_ui_app::widget::DescribedList {
    let mut props = vec![("in_column", Value::Bool(true))];
    if let Some(title) = title {
        props.push(("ungrouped_title", Value::Text(title.into())));
    }
    let items = items
        .iter()
        .map(|(key, group)| given(key, &[("group", Value::Text((*group).into()))]))
        .collect();
    let mut source = whole_list(&props, items);
    let props = tilo_ui_app::widget::VirtualListProps::from_source(&mut source).expect("reads");
    <tilo_ui_app::widget::DescribedList as Described>::from_props("list", props)
}

fn rendered_list(
    items: Vec<Item>,
) -> (
    tilo_ui_app::widget::DescribedList,
    tilo_ui_base::Intents<Fired>,
) {
    let (list, queue, _) = rendered_list_and_rows(items);
    (list, queue)
}

fn rendered_list_and_rows(
    items: Vec<Item>,
) -> (
    tilo_ui_app::widget::DescribedList,
    tilo_ui_base::Intents<Fired>,
    std::rc::Rc<std::cell::RefCell<Rendered>>,
) {
    let queue = tilo_ui_base::Intents::default();
    let rendered = std::rc::Rc::new(std::cell::RefCell::new(Rendered {
        items,
        asked: Vec::new(),
        queue: queue.clone(),
        changed: Vec::new(),
    }));
    let rows: SharedRows = rendered.clone();
    let mut source = Node {
        queue: queue.clone(),
        every_event: true,
        ..given(
            "list",
            &[
                ("selection_follows_focus", Value::Bool(true)),
                ("in_column", Value::Bool(true)),
            ],
        )
    }
    .rows(rows);
    let props = tilo_ui_app::widget::VirtualListProps::from_source(&mut source).expect("reads");
    (
        <tilo_ui_app::widget::DescribedList as Described>::from_props("list", props),
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

tilo_test_env::scenario! {
    fn a_described_list_reads_rows_groups_and_measure_from_its_host;

    phase a_list_of_rendered_rows_reads_them_from_its_host_and_its_menu_from_the_row;
    {
        let (mut list, queue) = rendered_list(vec![
            item("a", "Q3 plan", false),
            item("b", "Groceries", false),
        ]);
        let virtual_list = list.list_mut().expect("the list");
        assert_eq!(tilo_ui_app::widget::ListModel::len(virtual_list.model()), 2);
        assert_eq!(
            tilo_ui_app::widget::ListModel::label(virtual_list.model(), 1).as_deref(),
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
            .map(tilo_ui_app::widget::MenuItem::label)
            .collect();
        assert_eq!(labels, vec![Some("Delete")]);
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

    phase a_rendered_row_that_changes_keeps_the_windows_tree;
    {
        let (list, _queue, rows) = rendered_list_and_rows(vec![
            item("a", "Q3 plan", false),
            item("b", "Groceries", false),
        ]);
        let mut harness = tilo_ui_app::headless::Driver::for_root(
            tilo_ui_app::State::new(),
            list,
            (400, 300),
            tilo_ui_base::theme::Theme::dark(),
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

    phase a_page_column_holds_the_readable_measure_from_its_first_row;
    {
        fn pages<'a>(
            node: &'a tilo_ui_app::headless::InspectionNode,
            out: &mut Vec<&'a tilo_ui_app::headless::InspectionNode>,
        ) {
            if node.widget == "Page" {
                out.push(node);
            }
            for child in &node.children {
                pages(child, out);
            }
        }

        let mut source = whole_list(
            &[
                ("page_column", Value::Bool(true)),
                ("at_measure", Value::Bool(true)),
            ],
            ["a", "b", "c"]
                .iter()
                .map(|key| given(key, &[("title", Value::Text((*key).into()))]))
                .collect(),
        );
        let props = tilo_ui_app::widget::VirtualListProps::from_source(&mut source).expect("reads");
        let list = <tilo_ui_app::widget::DescribedList as Described>::from_props("list", props);
        let mut harness = tilo_ui_app::headless::Driver::for_root(
            tilo_ui_app::State::new(),
            list,
            (1600, 900),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let inspection = harness.inspection();
        let mut found = Vec::new();
        pages(&inspection.root, &mut found);
        assert_eq!(found.len(), 3, "one page row per message");
        let measure = f64::from(tilo_ui_app::widget::arrange::MEASURE_REM)
            * f64::from(tilo_ui_app::widget::arrange::rem_of(&tilo_ui_base::theme::Theme::dark()));
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

    phase rows_that_name_no_group_stand_under_their_own_heading;
    {
        use tilo_ui_app::widget::RowGroup;
        let list = grouped_list(
            Some("No date"),
            &[("a", "Jul 1"), ("b", ""), ("c", "Jul 1")],
        );
        let model = list.list().expect("the list").model();
        assert_eq!(
            tilo_ui_app::widget::ListModel::group_at(model, 0),
            Some(RowGroup::heading("Jul 1"))
        );
        assert_eq!(
            tilo_ui_app::widget::ListModel::group_at(model, 1),
            Some(RowGroup::heading("No date")),
            "a row whose item names no group is not the last group's"
        );
        let list = grouped_list(None, &[("a", "Jul 1"), ("b", "")]);
        let model = list.list().expect("the list").model();
        assert_eq!(tilo_ui_app::widget::ListModel::group_at(model, 1), None);
        let mut source = whole_list(
            &[("ungrouped_title", Value::Text("No date".into()))],
            vec![given(
                "a",
                &[
                    ("group", Value::Text("Jul 1".into())),
                    ("removed", Value::Bool(true)),
                ],
            )],
        );
        let props = tilo_ui_app::widget::VirtualListProps::from_source(&mut source).expect("reads");
        let list = <tilo_ui_app::widget::DescribedList as Described>::from_props("list", props);
        assert_eq!(
            tilo_ui_app::widget::ListModel::group_at(list.list().expect("the list").model(), 0),
            None
        );
    }
}

/// Someone a view shows wears the colour their workspace paints them in, carried with them; a
/// sender that knows no members leaves it to the rule every surface derives it by.
#[test]
fn a_person_wears_the_colour_that_travels_with_them() {
    let identity: tilo_actions::Actor = tilo_actions::MemberId::agent("planner", "ada").into();
    let worn = |hue| {
        Person::from_value(&Value::Actor {
            identity: identity.clone(),
            name: "Planner".into(),
            hue,
            picture: None,
        })
        .unwrap()
        .hue
    };
    assert_eq!(worn(Some(1)), tilo_ui_base::tokens::PresenceHue::Orange);
    assert_eq!(
        worn(None),
        tilo_ui_base::tokens::PresenceHue::of_member(&identity)
    );
}

/// A component whose press is bound to an action says why it cannot be pressed now. The runtime
/// dims the node whose `press` is bound to an action whose `require` does not hold and gives the
/// node `enabled` and the reason as `description`, so every component that takes a press must
/// take both, or the app it stands in fails to open the moment its press is refused. Read over
/// the registry, so a component that forgets either fails here the day it is written rather than
/// in a person's window.
#[test]
fn every_component_that_takes_a_press_takes_the_reason_it_cannot_run() {
    let catalogue = toolkit().catalogue();
    let mut missing = Vec::new();
    for component in &catalogue.components {
        let Some(press) = component.events.iter().find(|event| event.name == "press") else {
            continue;
        };
        if press.value.is_none() && !press.press {
            missing.push(format!(
                "{}: its press runs a bound action, so declare it `#[component(event(press))]`",
                component.name
            ));
        }
        for prop in ["enabled", "description"] {
            if component.prop(prop).is_none() {
                missing.push(format!(
                    "{}.{prop}: a component whose press a view binds an action to takes it, or its \
                     app cannot open while that action is refused",
                    component.name
                ));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
