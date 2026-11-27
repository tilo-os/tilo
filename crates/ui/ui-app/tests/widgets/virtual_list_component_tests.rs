use std::cell::*;
use std::collections::*;
use std::ops::Range;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::arrange::{Align, Row, Stack};
use tilo_ui_app::widget::internals::virtual_list_component::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::collection_transition::ListModelChange;
use tilo_ui_base::component::Component;
use tilo_ui_base::component::Event;

use tilo_ui_base::a11y::*;
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

/// The rows of a list given its items whole: a row a title each.
fn given_rows(count: usize) -> DescribedRows {
    DescribedRows::given(
        (0..count)
            .map(|at| ListItemProps {
                key: format!("row-{at}"),
                row: ItemRow::Row(Box::new(ListRowProps {
                    title: format!("row {at}"),
                    ..ListRowProps::default()
                })),
                ..ListItemProps::default()
            })
            .collect(),
    )
}

/// The props of a list of `rows` that is a column's whole body.
fn column_of(rows: DescribedRows) -> VirtualListProps {
    VirtualListProps {
        in_column: true,
        rows,
        ..VirtualListProps::default()
    }
}

/// Lay the list out in a column of `height`, as the window does.
fn lay_out(list: &mut VirtualList<DescribedRows>, height: f32) {
    lay_out_tree(list, height);
}

/// Lay the list out in a column of `height` and give back the tree it was laid out in, for
/// the events sent to it afterwards.
fn lay_out_tree(
    list: &mut VirtualList<DescribedRows>,
    height: f32,
) -> (taffy::TaffyTree<()>, taffy::NodeId) {
    list.set_viewport_height(height);
    let doc = automerge::AutoCommit::new();
    Widget::sync(list, &doc, &automerge::ROOT);
    let mut taffy = taffy::TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    let node = Widget::build_layout(
        list,
        &mut taffy,
        &tilo_ui_base::theme::Theme::dark(),
        &mut nodes,
    );
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(300.0),
                height: taffy::AvailableSpace::Definite(height),
            },
        )
        .unwrap();
    Widget::apply_layout(list, &taffy, node, Point::ZERO);
    (taffy, node)
}

/// The items of a list where each carries a document of its own: they open where they
/// stand.
fn rows_with_documents(count: usize) -> DescribedRows {
    let items = (0..count)
        .map(|at| ListItemProps {
            key: format!("row-{at}"),
            item: Some(ResourceId(format!("task-{at}"))),
            row: ItemRow::Row(Box::new(ListRowProps {
                title: format!("row {at}"),
                ..ListRowProps::default()
            })),
            detail: ItemRow::Row(Box::new(ListRowProps {
                title: format!("what row {at} is about"),
                secondary: Some("with the notes written about it".to_owned()),
                ..ListRowProps::default()
            })),
            ..ListItemProps::default()
        })
        .collect();
    DescribedRows::test_holding(Held::Given(Rc::new(items)))
}

/// The rows of a list whose items offer on their menu the one command their type offers:
/// each opens where it stands when `opens_here`, and elsewhere when not.
fn rows_offering(count: usize, opens_here: bool) -> DescribedRows {
    let items = (0..count)
        .map(|at| ListItemProps {
            key: format!("row-{at}"),
            item: Some(ResourceId(format!("task-{at}"))),
            row: ItemRow::Row(Box::new(ListRowProps {
                title: format!("row {at}"),
                ..ListRowProps::default()
            })),
            detail: if opens_here {
                ItemRow::Row(Box::new(ListRowProps {
                    title: format!("what row {at} is about"),
                    ..ListRowProps::default()
                }))
            } else {
                ItemRow::None
            },
            menu: vec![MenuPart::Item(MenuItemProps {
                label: "Complete".to_owned(),
                ..MenuItemProps::default()
            })],
            ..ListItemProps::default()
        })
        .collect();
    DescribedRows::test_holding(Held::Given(Rc::new(items)))
}

/// Lay `list` out in a column of `height` and press `button` over the row `index`, as a
/// person does: the left button to tap the row, the right one to see what its item offers
/// there.
fn press_row(
    list: &mut DescribedList,
    index: usize,
    button: winit::event::MouseButton,
    height: f32,
) {
    let (taffy, node) = {
        let inner = list.list_mut().expect("the list stands");
        lay_out_tree(inner, height)
    };
    let row = list
        .list()
        .and_then(|list| list.row_rect(&format!("row-{index}")))
        .expect("the row is laid out");
    let at = Point::new(20.0, row.center().y);
    let pressed = winit::event::ElementState::Pressed;
    let mut events = vec![
        tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y),
        tilo_ui_base::synthetic_input::mouse_input(pressed, button),
    ];
    if button == winit::event::MouseButton::Left {
        events.push(tilo_ui_base::synthetic_input::mouse_input(
            winit::event::ElementState::Released,
            button,
        ));
    }
    for event in &events {
        tilo_ui_app::testing::Dispatch::new()
            .layout(&taffy, node)
            .send(list, event);
    }
}

/// The messages of a transcript, two of one day and two of the next: each shows a line of
/// its own, under its author and the time it was said, and says who has read to it.
fn transcript_messages(read_by: impl Fn(usize) -> Vec<Person>) -> DescribedRows {
    let at = [
        1_790_000_000_i64,
        1_790_000_060,
        1_790_090_000,
        1_790_090_060,
    ];
    let items = at
        .iter()
        .enumerate()
        .map(|(index, said)| ListItemProps {
            key: format!("row-{index}"),
            item: Some(ResourceId(format!("message-{index}"))),
            author: Some(Person {
                identity: tilo_actions::MemberId::person(format!("person-{index}")).into(),
                name: format!("Writer {index}"),
                hue: tilo_ui_base::tokens::PresenceHue::of_identity(&format!("person-{index}")),
                picture: None,
            }),
            time: Some(Moment(*said)),
            read_by: read_by(index),
            row: ItemRow::Row(Box::new(ListRowProps {
                title: format!("message {index}"),
                ..ListRowProps::default()
            })),
            ..ListItemProps::default()
        })
        .collect();
    DescribedRows::test_holding(Held::Given(Rc::new(items)))
}

/// What `list` lays out, as the inspection sees it: where each widget stands.
fn laid_out(list: DescribedList) -> tilo_ui_app::headless::InspectionNode {
    laid_out_in(list, tilo_ui_base::theme::Theme::dark())
}

/// What `list` lays out under `theme`, as the inspection sees it.
fn laid_out_in(
    list: DescribedList,
    theme: tilo_ui_base::theme::Theme,
) -> tilo_ui_app::headless::InspectionNode {
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        list,
        (900, 600),
        theme,
    );
    harness.relayout();
    harness.inspection().root
}

/// The names the faces of `widget` say, in `node` and under it.
fn find_widget(node: &tilo_ui_app::headless::InspectionNode, widget: &str, out: &mut Vec<String>) {
    if node.widget == widget {
        out.push(
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.clone())
                .unwrap_or_default(),
        );
    }
    for child in &node.children {
        find_widget(child, widget, out);
    }
}

/// Who has read to a line shows on that line alone: each reader's face stands once, on the
/// message their reading ends at, and the author never beside their own words.
#[test]
fn a_readers_face_stands_once_on_the_line_they_read_to() {
    let list = DescribedList::from_props(
        "transcript",
        VirtualListProps {
            page_column: true,
            rows: transcript_messages(|index| match index {
                // // Mia read to the second message; the second message's own author read it // too, and never
                // shows beside their own words.
                1 => vec![
                    Person {
                        identity: tilo_actions::MemberId::person("person-0").into(),
                        name: "Mia".to_owned(),
                        hue: tilo_ui_base::tokens::PresenceHue::of_identity("person-0"),
                        picture: None,
                    },
                    Person {
                        identity: tilo_actions::MemberId::person("person-1").into(),
                        name: "Writer 1".to_owned(),
                        hue: tilo_ui_base::tokens::PresenceHue::of_identity("person-1"),
                        picture: None,
                    },
                ],
                _ => Vec::new(),
            }),
            ..VirtualListProps::default()
        },
    );
    let laid = laid_out(list);
    let faces = |node: &tilo_ui_app::headless::InspectionNode, at: usize| {
        let rows = node
            .children
            .iter()
            .filter(|child| child.widget == "Page")
            .collect::<Vec<_>>();
        let mut found = Vec::new();
        find_widget(rows[at], "Avatar", &mut found);
        found
    };
    assert_eq!(
        faces(&laid, 1),
        vec!["Writer 1".to_owned(), "Mia".to_owned()],
        "the line Mia read to shows her face beside its author's, once"
    );
    for at in [0, 2, 3] {
        let shown = faces(&laid, at);
        assert_eq!(
            shown.len(),
            1,
            "message {at} shows its author's face alone: {shown:?}"
        );
    }
}

/// A page column's rows are read, not chosen: a press on a message gives it the keyboard
/// and leaves nothing tinted behind, and the keyboard's way through the rows moves (and
/// rings) the row it is on without choosing that one either. A column's rows stand for
/// what a window opens, and are chosen as they always were.
#[test]
fn a_press_on_a_transcript_row_chooses_nothing() {
    let left = winit::event::MouseButton::Left;
    let mut transcript = DescribedList::from_props(
        "transcript",
        VirtualListProps {
            follow_end: true,
            page_column: true,
            rows: given_rows(6),
            ..VirtualListProps::default()
        },
    );
    press_row(&mut transcript, 1, left, 400.0);
    let list = transcript.list_mut().expect("the list stands");
    assert_eq!(
        list.selection_len(),
        0,
        "a message is read, not chosen: nothing stays selected"
    );
    assert_eq!(
        list.focused_key().map(String::as_str),
        Some("row-1"),
        "the row the person pressed takes the keyboard"
    );
    let (taffy, node) = lay_out_tree(list, 400.0);
    Widget::set_focus(list, true);
    let up = tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowUp);
    assert!(
        tilo_ui_app::testing::Dispatch::new()
            .layout(&taffy, node)
            .send(list, &up)
            .handled,
        "the list follows the keyboard"
    );
    assert_eq!(
        list.focused_key().map(String::as_str),
        Some("row-0"),
        "the keyboard moved up a row"
    );
    assert_eq!(list.selection_len(), 0, "no row was chosen on the way");

    // // A column's rows are chosen, as a list of things to open always was.
    let mut column = DescribedList::from_props("column", column_of(given_rows(6)));
    press_row(&mut column, 1, left, 400.0);
    let list = column.list().expect("the list stands");
    assert_eq!(list.selection_len(), 1, "a column's rows are chosen");
}

/// The change a person has not seen is marked on the item, whether the item stands as a row
/// or as the page its row opened into: the page takes the row's place, so the mark goes with
/// the item rather than vanishing with the row.
#[test]
fn the_page_a_row_opens_into_carries_the_mark_of_the_change() {
    fn rows_of(root: &dyn tilo_ui_base::widget::Widget, out: &mut Vec<String>) {
        if let Some(row) = root
            .as_any()
            .and_then(|widget| widget.downcast_ref::<ListRow>())
            && let Some(change) = row.shown_change()
        {
            out.push(change.said.to_owned());
        }
        for child in root.children() {
            rows_of(child, out);
        }
    }
    let items: Vec<ListItemProps> = (0..3)
        .map(|at| ListItemProps {
            key: format!("row-{at}"),
            item: Some(ResourceId(format!("task-{at}"))),
            row: ItemRow::Row(Box::new(ListRowProps {
                title: format!("row {at}"),
                ..ListRowProps::default()
            })),
            detail: ItemRow::Row(Box::new(ListRowProps {
                title: format!("what row {at} is about"),
                secondary: Some("with the notes written about it".to_owned()),
                ..ListRowProps::default()
            })),
            change: (at == 1).then(|| ChangeMarkProps {
                by: Some(Person {
                    identity: tilo_actions::MemberId::person("bo").into(),
                    name: "Bo".to_owned(),
                    hue: tilo_ui_base::tokens::PresenceHue::of_identity("bo"),
                    picture: None,
                }),
                said: Some("moved it to Today".to_owned()),
                ..ChangeMarkProps::default()
            }),
            ..ListItemProps::default()
        })
        .collect();
    let list = DescribedList::from_props(
        "list",
        VirtualListProps {
            open: Some(ResourceId("task-1".to_owned())),
            rows: DescribedRows::test_holding(Held::Given(Rc::new(items))),
            ..VirtualListProps::default()
        },
    );
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        list,
        (900, 600),
        tilo_ui_base::theme::Theme::dark(),
    );
    harness.relayout();
    let mut marked = Vec::new();
    rows_of(harness.root_widget(), &mut marked);
    assert_eq!(
        marked,
        vec!["Bo moved it to Today".to_owned()],
        "the page the row became carries the mark of the change, in its words"
    );
}

/// A menu opened over a row that opens where it stands is about that row alone: choosing
/// the row shows its item under it, so opening the menu reports no choice of it for the
/// window to open the item elsewhere.
#[test]
fn a_menu_over_a_row_that_opens_in_place_reports_no_choice() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let select = Event::View(Intent::new(&queue, "list", "select"));
    let props = VirtualListProps {
        select,
        ..column_of(rows_offering(4, true))
    };
    let mut list = DescribedList::from_props("list", props);
    press_row(&mut list, 1, winit::event::MouseButton::Right, 400.0);
    assert!(
        list.menu_area_mut()
            .as_deref()
            .is_some_and(ContextMenuArea::is_open),
        "the row's menu opens where the press was"
    );
    assert!(
        queue.take().is_empty(),
        "the list reports no choice of a row it opens where it stands"
    );

    let props = VirtualListProps {
        select: Event::View(Intent::new(&queue, "list", "select")),
        ..column_of(rows_offering(4, false))
    };
    let mut plain = DescribedList::from_props("list", props);
    press_row(&mut plain, 1, winit::event::MouseButton::Right, 400.0);
    assert!(
        plain
            .menu_area_mut()
            .as_deref()
            .is_some_and(ContextMenuArea::is_open),
        "the row's menu opens where the press was"
    );
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "the menu chooses the row: {fired:?}");
    assert_eq!(fired[0].event, "select");

    let select = Event::View(Intent::new(&queue, "list", "select"));
    let props = VirtualListProps {
        select,
        narrow: true,
        ..column_of(rows_offering(4, true))
    };
    let mut front = DescribedList::from_props("list", props);
    press_row(&mut front, 1, winit::event::MouseButton::Right, 400.0);
    let fired = queue.take();
    assert_eq!(
        fired.len(),
        1,
        "the menu chooses a row whose item opens in front: {fired:?}"
    );
    assert_eq!(fired[0].event, "select");
}

/// A row whose item offers a menu the host has not built yet is asked for it, and the menu
/// the person opened waits for that answer: once it comes, the commands the row's own view
/// placed and the commands its item offers are listed together, rather than a menu holding
/// only the question every menu carries.
#[test]
fn a_row_whose_menu_is_asked_for_lists_its_own_and_its_items_commands() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // A shell that can carry a question is connected, as in an app window: every menu then
    // keeps its "Send to…" row, so an empty one opens rather than being held back.
    tilo_ui_app::shared_selection::set_send_host_available(true);
    // The host answers the ask for the row's menu when the test says so, as an app does a
    // frame or two later.
    let answered = Rc::new(Cell::new(false));
    let list = DescribedList::from_props(
        "list",
        column_of(DescribedRows::test_holding(Held::Rendered(Rc::new(
            RefCell::new(
                ViewRows::new(&[("row-0", None), ("row-1", None)], 0.0, 0.0)
                    .answering(Rc::clone(&answered)),
            ),
        )))),
    );
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        list,
        (900, 600),
        tilo_ui_base::theme::Theme::dark(),
    );
    harness.relayout();
    let key = "row-1".to_owned();
    let row = harness
        .root::<DescribedList>()
        .list()
        .and_then(|list| list.row_rect(&key))
        .expect("the row is laid out");
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
        20.0,
        row.center().y,
    ));
    harness.send(&tilo_ui_base::synthetic_input::mouse_input(
        winit::event::ElementState::Pressed,
        winit::event::MouseButton::Right,
    ));
    assert!(
        !harness
            .root_mut::<DescribedList>()
            .menu_area_mut()
            .as_deref()
            .is_some_and(ContextMenuArea::is_open),
        "the menu waits for the row's commands instead of opening on its question"
    );
    // The host answers: the row now carries what its item offers, and the list takes it.
    answered.set(true);
    harness.relayout();
    let mut root = harness.root_mut::<DescribedList>();
    let area = root.menu_area_mut().expect("the list stands");
    assert!(
        area.is_open(),
        "the menu opens once what it offers has come"
    );
    let labels: Vec<&str> = area
        .menu()
        .items()
        .iter()
        .filter_map(tilo_ui_app::widget::MenuItem::label)
        .collect();
    assert_eq!(
        labels.get(..3),
        Some(["Open as page", "Complete", "Delete"].as_slice()),
        "the row's own command first, then its item's, as a person reads them: {labels:?}"
    );
    tilo_ui_app::shared_selection::set_send_host_available(false);
}

/// The item a host holds open stands open where it stands: the list it builds again shows
/// it as the person left it, and an item it does not hold open does not.
#[test]
fn the_item_a_host_holds_open_stands_open() {
    let props = VirtualListProps {
        open: Some(ResourceId("task-2".into())),
        ..column_of(rows_with_documents(4))
    };
    let mut list = DescribedList::from_props("list", props);
    let inner = list.list_mut().expect("the list stands");
    assert_eq!(
        inner.open_row().map(String::as_str),
        Some("row-2"),
        "the item the host holds open is the row held open"
    );
    lay_out(inner, 400.0);
    // // The item's document stands in the row's place: the rows over it keep their parts, // and the
    // rows under it follow where the document ends, never one row above // it. What the row
    // writes there is the layout check's to read (`repeated_row_title`), // which watches a title
    // written in a row and again in the item it opened into.
    let row_top = inner.test_viewport().item_top(2).unwrap_or_default();
    let above = inner.test_viewport().item_top(1).unwrap_or_default()
        + inner.test_viewport().item_height(1).unwrap_or_default();
    assert_eq!(
        row_top, above,
        "the document stands where the row stood: {row_top} over the row's {above}"
    );
    assert_eq!(
        inner.row_box(2).map(|row| row.y0),
        Some(row_top),
        "the row drawn for the item starts where its part does"
    );
    let below = row_top + inner.test_viewport().item_height(2).unwrap_or_default();
    assert_eq!(
        inner.test_viewport().item_top(3),
        Some(below),
        "the rows under the item follow where its document ends"
    );
    let same = VirtualListProps {
        open: Some(ResourceId("task-2".into())),
        ..column_of(rows_with_documents(4))
    };
    assert!(
        list.set_props(same),
        "the list takes its items where it stands"
    );
    assert_eq!(
        list.list()
            .and_then(VirtualList::open_row)
            .map(String::as_str),
        Some("row-2")
    );
    let closed = VirtualListProps {
        open: None,
        ..column_of(rows_with_documents(4))
    };
    assert!(
        list.set_props(closed),
        "the list takes its items where it stands"
    );
    assert_eq!(list.list().and_then(VirtualList::open_row), None);
}

/// A row dropped in another place is reported as the two ids it moved between: the app keeps
/// its own order, and the list never edits a model it does not own.
#[test]
fn a_reordered_row_reports_the_two_ids_it_moved_between() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let reorder = Event::View(Intent::new(&queue, "list", "reorder"));
    let mut list = DescribedList::from_props(
        "list",
        VirtualListProps {
            reorder,
            ..column_of(rows_with_documents(3))
        },
    );
    let inner = list.list_mut().expect("the list stands");
    let (taffy, node) = lay_out_tree(inner, 400.0);
    Widget::set_focus(inner, true);
    inner.focus_row("row-0".to_owned());
    let down = tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowDown);
    assert!(
        tilo_ui_app::testing::Dispatch::new()
            .modifiers(winit::keyboard::ModifiersState::CONTROL)
            .layout(&taffy, node)
            .send(inner, &down)
            .handled,
        "the list takes the chord"
    );
    let fired: Vec<Fired> = queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "reorder")
        .collect();
    assert_eq!(fired.len(), 1, "the host hears the move once");
    match &fired[0].value {
        Some(Value::List(items)) => assert_eq!(
            items,
            &vec![
                Value::Text("row-0".to_owned()),
                Value::Text("row-1".to_owned())
            ],
            "the row and where it went"
        ),
        other => panic!("the move carries the two ids: {other:?}"),
    }
}

/// The person opening and closing the row they read is reported as the item it shows, so a
/// host keeping it can put it back.
#[test]
fn the_person_opening_a_row_reports_the_item_it_shows() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let opened = Event::View(Intent::new(&queue, "list", "opened"));
    let closed = Event::View(Intent::new(&queue, "list", "closed"));
    let props = VirtualListProps {
        opened: opened.clone(),
        closed: closed.clone(),
        ..column_of(rows_with_documents(4))
    };
    let mut list = DescribedList::from_props("list", props);
    let inner = list.list_mut().expect("the list stands");
    lay_out(inner, 400.0);
    inner.activate_key(tilo_ui_app::widget::ListKey::new("row-1".to_owned()));
    assert_eq!(inner.open_row().map(String::as_str), Some("row-1"));
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "the host hears one event: {fired:?}");
    assert_eq!(fired[0].event, "opened");
    assert!(
        matches!(fired[0].value.clone(), Some(Value::Resource(id)) if id == "task-1"),
        "the host hears the item the person opened: {fired:?}"
    );
    inner.close_open();
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "the host hears it close: {fired:?}");
    assert_eq!(fired[0].event, "closed");
}

/// A list given its items again keeps where the person scrolled: the items are replaced
/// where the list stands, not the list built again around them.
#[test]
fn a_list_given_its_items_again_keeps_where_the_person_scrolled() {
    let mut list = DescribedList::from_props("list", column_of(given_rows(200)));
    {
        let inner = list.list_mut().expect("the list stands");
        lay_out(inner, 200.0);
        lay_out(inner, 200.0);
        inner.set_scroll_offset(600.0);
        assert!(inner.scroll_offset() > 0.0, "the list scrolled");
    }
    let scrolled = list.list().expect("the list stands").scroll_offset();
    let props = VirtualListProps {
        tail_title: Some("Done".into()),
        ..column_of(given_rows(200))
    };
    assert!(
        list.set_props(props),
        "the list takes its items where it stands"
    );
    let inner = list.list().expect("the list stands");
    assert!(
        (inner.scroll_offset() - scrolled).abs() < 0.01,
        "the rows stayed where the person scrolled them: {} then {}",
        scrolled,
        inner.scroll_offset()
    );
}

/// A row the host has named but not rendered is on its way: the list keeps its part, of the
/// height the rows around it take, with a placeholder there rather than a row of nothing.
#[test]
fn a_row_the_host_has_not_rendered_is_on_its_way() {
    let rendered: SharedRows = Rc::new(RefCell::new(
        ViewRows::new(&[("row-0", None), ("row-1", None)], 0.0, 0.0).withholding(&["row-1"]),
    ));
    let rows = DescribedRows::test_holding(Held::Rendered(rendered));
    assert_eq!(rows.key_at(1).as_deref(), Some("row-1"));
    assert!(
        rows.row_state(0).ready().is_some(),
        "the row the host rendered is there"
    );
    assert!(
        rows.row_state(1).is_pending(),
        "the row it has not rendered is on its way"
    );
}

/// Rows a host renders as they show, arriving after the list was built: the list of a
/// collection an app reads page by page holds no row until its first page comes.
struct Arriving {
    keys: Rc<RefCell<Vec<String>>>,
}

impl tilo_ui_base::component::RowSource for Arriving {
    fn len(&self) -> usize {
        self.keys.borrow().len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.keys.borrow().get(index).cloned()
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.keys.borrow().iter().position(|here| here == key)
    }

    fn request_range(&mut self, _range: Range<usize>, _open: Option<String>) {}

    fn ask_menu(&mut self, _key: &str) {}

    fn rendered(&self, key: &str) -> bool {
        self.keys.borrow().iter().any(|here| here == key)
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        if !self.rendered(key) {
            return Ok(false);
        }
        let mut rows = ViewRows::new(&[(key, None)], 0.0, 0.0);
        tilo_ui_base::component::RowSource::read(&mut rows, key, read)
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        self.keys
            .borrow()
            .iter()
            .find(|key| ViewRows::item_of(key) == id)
            .cloned()
    }

    /// The host's first page fills the model without a change to report: a list given its
    /// rows for the first time hears nothing.
    fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
        Vec::new()
    }

    fn has_changes(&self) -> bool {
        false
    }
}

/// A list whose host renders its rows as they show takes the host's choice once the row it
/// names arrives, though it was built while there was no row to choose and its host reports
/// no change when the rows come.
#[test]
fn a_hosts_choice_is_taken_when_the_row_it_names_arrives() {
    let keys = Rc::new(RefCell::new(Vec::new()));
    let rendered: SharedRows = Rc::new(RefCell::new(Arriving {
        keys: Rc::clone(&keys),
    }));
    let props = VirtualListProps {
        in_column: true,
        chosen: Some(vec![ResourceId("item-0".to_owned())]),
        rows: DescribedRows::test_holding(Held::Rendered(rendered)),
        ..VirtualListProps::default()
    };
    let mut list = DescribedList::from_props("list", props);
    {
        let inner = list.list_mut().expect("the list stands");
        lay_out(inner, 400.0);
        assert_eq!(
            inner.selection_len(),
            0,
            "the host's rows are not there to choose from yet"
        );
    }
    // The host's first page of rows arrives.
    keys.borrow_mut().push("row-0".to_owned());
    let inner = list.list_mut().expect("the list stands");
    lay_out(inner, 400.0);
    assert_eq!(
        inner.selection_len(),
        1,
        "the row the host chose is selected"
    );
    assert!(
        inner.selection().is_selected(&"row-0".to_owned()),
        "the row showing item-0 is the one chosen"
    );
}

/// The rows in place of items someone else removed are the list's own: the host that
/// renders the rows it shows, one at a time, has never rendered them and never will. A list
/// showing one row builds such a row beside the one it holds, struck, rather than leaving
/// its part on its way.
#[test]
fn a_row_in_place_of_a_removed_item_is_built_beside_the_hosts_rows() {
    let props =
        |removed: Vec<ListItemProps>| VirtualListProps {
            in_column: true,
            rows: DescribedRows::test_holding(Held::Rendered(Rc::new(RefCell::new(
                ViewRows::new(&[("row-0", None)], 0.0, 0.0),
            )))),
            removed: RemovedRows(Rc::new(removed)),
            ..VirtualListProps::default()
        };
    let mut list = DescribedList::from_props("list", props(Vec::new()));
    let inner = list.list_mut().expect("the list stands");
    lay_out(inner, 400.0);
    assert_eq!(inner.mounted_len(), 1, "one row shows");
    assert!(
        inner.model().row_state(0).ready().is_some(),
        "the row the host rendered is there"
    );
    let mut list = DescribedList::from_props(
        "list",
        props(vec![ListItemProps {
            key: "row-gone".to_owned(),
            removed: true,
            row: ItemRow::Row(Box::new(ListRowProps {
                title: "Book the dentist".to_owned(),
                ..ListRowProps::default()
            })),
            ..ListItemProps::default()
        }]),
    );
    let inner = list.list_mut().expect("the list stands");
    lay_out(inner, 400.0);
    assert_eq!(inner.model().len(), 2, "the model grew by the removed row");
    assert!(
        inner.model().row_state(1).ready().is_some(),
        "a row the list holds itself is no row on its way"
    );
    assert_eq!(
        inner.mounted_rows().collect::<Vec<_>>(),
        [0, 1],
        "both rows are built"
    );
    let semantics = inner.accessibility().expect("the list has a node");
    let listed: Vec<Option<&str>> = semantics
        .children
        .iter()
        .map(|row| row.name.as_deref())
        .collect();
    assert_eq!(
        listed,
        [Some("row-0's words"), Some("Book the dentist")],
        "the removed item's row is built as a row, where it stands"
    );
}

/// A menu command that reports its choose to `queue` when run.
fn command(label: &str, key: &str, queue: &tilo_ui_base::Intents<Fired>) -> MenuPart {
    let mut item = MenuItemProps {
        label: label.to_owned(),
        key: Some(key.to_owned()),
        ..MenuItemProps::default()
    };
    item.choose = Event::View(Intent::new(queue, "row", "choose"));
    MenuPart::Item(item)
}

/// The row's menus reach it with the keys they declare, matched as every shortcut is.
#[test]
fn a_row_reads_the_shortcuts_its_menu_declares() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let menu = vec![command("Complete", "Ctrl+Enter", &queue)];
    let attached = vec![
        command("Double click", "Double click", &queue),
        MenuPart::Separator,
    ];
    let shortcuts = declared_shortcuts(&menu, &attached, (&[], &Event::None));
    assert_eq!(shortcuts.len(), 1, "text that is no chord names no key");
    assert!(
        shortcuts[0]
            .0
            .same_press(&tilo_ui_base::platform::Shortcut::from("Control+Return"))
    );
    (shortcuts[0].1)();
    assert_eq!(
        queue.take(),
        [Fired {
            key: "row".to_owned(),
            event: "choose".to_owned(),
            value: None,
            gesture: None,
        }]
    );
}

/// One key runs one command: the first the menus offer it on holds it.
#[test]
fn one_key_sessions_one_command_of_the_row() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let menu = vec![command("Complete", "Ctrl+Enter", &queue)];
    let attached = vec![command("Reopen", "control+enter", &queue)];
    assert_eq!(
        declared_shortcuts(&menu, &attached, (&[], &Event::None)).len(),
        1
    );
}

/// One item of a list a test renders row by row: its key, and the item it stands under.
struct ViewItem {
    key: String,
    parent: Option<String>,
}

/// The items a test renders as the views an app builds: a list of a collection of any length
/// holds its rows that way, and each item's document leads its line out as a row does.
struct ViewRows {
    items: Vec<ViewItem>,
    /// The rows the host has named but not rendered: they are on their way.
    withheld: Vec<String>,
    /// The row whose menu the host asked for: the app answers with the commands its item
    /// offers, which the row did not carry before.
    asked: Option<String>,
    /// Whether the host has answered the ask: the asked row then carries its item's menus.
    answered: Rc<Cell<bool>>,
    /// The width a row's leading control takes, and the gap after it.
    lead: f32,
    gap: f32,
    /// Whether a row is a view of the app's own rather than one list row: the two renderings
    /// a keyed item answers alike.
    views: bool,
    /// The keys the item's actions answer to.
    keys: Vec<String>,
    /// What the item's `shortcut` event runs, when it names what a press reports.
    shortcut: Event,
}

impl ViewRows {
    fn new(items: &[(&str, Option<&str>)], lead: f32, gap: f32) -> Self {
        Self {
            items: items
                .iter()
                .map(|(key, parent)| ViewItem {
                    key: (*key).to_owned(),
                    parent: parent.map(str::to_owned),
                })
                .collect(),
            withheld: Vec::new(),
            asked: None,
            answered: Rc::new(Cell::new(false)),
            lead,
            gap,
            views: false,
            keys: Vec::new(),
            shortcut: Event::None,
        }
    }

    /// The same rows, each rendered as a view of the app's own.
    fn with_views(mut self) -> Self {
        self.views = true;
        self
    }

    /// The same rows, their items answering `keys`, each running `event`.
    fn keys(mut self, keys: &[&str], event: Event) -> Self {
        self.keys = keys.iter().map(|key| (*key).to_owned()).collect();
        self.shortcut = event;
        self
    }

    /// The host answers the ask for a row's menu once `answered` is set.
    fn answering(mut self, answered: Rc<Cell<bool>>) -> Self {
        self.answered = answered;
        self
    }

    /// The same rows, the host having rendered every one but `keys`: those are on their way.
    fn withholding(mut self, keys: &[&str]) -> Self {
        self.withheld = keys.iter().map(|key| (*key).to_owned()).collect();
        self
    }

    /// The resource the item of the row `key` is.
    fn item_of(key: &str) -> String {
        key.replace("row-", "item-")
    }
}

impl tilo_ui_base::component::RowSource for ViewRows {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.items.get(index).map(|item| item.key.clone())
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.items.iter().position(|item| item.key == key)
    }

    fn request_range(&mut self, _range: Range<usize>, _open: Option<String>) {}

    fn ask_menu(&mut self, key: &str) {
        self.asked = Some(key.to_owned());
    }

    fn rendered(&self, key: &str) -> bool {
        !self.withheld.iter().any(|held| held == key)
            && self.items.iter().any(|item| item.key == key)
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        if !self.rendered(key) {
            return Ok(false);
        }
        let Some(item) = self.items.iter().find(|item| item.key == key) else {
            return Ok(false);
        };
        read(&mut ViewItemSource {
            item,
            menu: self.answered.get() && self.asked.as_deref() == Some(key),
            lead: self.lead,
            gap: self.gap,
            views: self.views,
            keys: self.keys.clone(),
            shortcut: self.shortcut.clone(),
        })?;
        Ok(true)
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        self.items
            .iter()
            .find(|item| ViewRows::item_of(&item.key) == id)
            .map(|item| item.key.clone())
    }

    fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
        Vec::new()
    }

    fn has_changes(&self) -> bool {
        false
    }
}

/// One item as the host renders it: its row, the document it opens into, and the menu the
/// host asked it for.
struct ViewItemSource<'a> {
    item: &'a ViewItem,
    /// The host asked for this row's menu: it carries the commands its item offers.
    menu: bool,
    lead: f32,
    gap: f32,
    /// The row is a view of the app's own, with more in it than one list row.
    views: bool,
    /// The keys the item's actions answer to.
    keys: Vec<String>,
    /// What a press on one of the item's keys reports.
    shortcut: Event,
}

impl Source for ViewItemSource<'_> {
    fn key(&self) -> &str {
        &self.item.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        match name {
            "item" => Some(Value::Resource(ViewRows::item_of(&self.item.key))),
            "parent" => self
                .item
                .parent
                .as_deref()
                .map(|parent| Value::Resource(ViewRows::item_of(parent))),
            // Until the host asks, the row says its item offers a menu it does not carry.
            "offers_menu" if !self.menu => Some(Value::Bool(true)),
            "keys" => Some(Value::List(
                self.keys.iter().cloned().map(Value::Text).collect(),
            )),
            _ => None,
        }
    }

    fn event(&mut self, name: &str) -> Event {
        if name == "shortcut" {
            return self.shortcut.clone();
        }
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, part: &str) -> Vec<Element> {
        match part {
            "row" if self.views => vec![view_row(&self.item.key)],
            "detail" => vec![document(&self.item.key, self.lead, self.gap)],
            _ => Vec::new(),
        }
    }

    fn part_kinds(&mut self, part: &str) -> Vec<Option<String>> {
        match part {
            "row" if self.views => vec![None],
            "row" => vec![Some(ListRowProps::NAME.to_owned())],
            "detail" => vec![None],
            "attached" => vec![Some(tilo_ui_app::widget::ContextMenuProps::NAME.to_owned())],
            "menu" if self.menu => vec![
                Some(MenuItemProps::NAME.to_owned()),
                Some(MenuItemProps::NAME.to_owned()),
            ],
            _ => Vec::new(),
        }
    }

    fn components(
        &mut self,
        part: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        match part {
            "row" => {
                read(&mut ViewRowSource {
                    key: &self.item.key,
                })?;
                Ok(1)
            }
            "attached" => {
                read(&mut ViewAttachedMenuSource)?;
                Ok(1)
            }
            "menu" if self.menu => {
                read(&mut ViewMenuItemSource { label: "Complete" })?;
                read(&mut ViewMenuItemSource { label: "Delete" })?;
                Ok(2)
            }
            _ => Ok(0),
        }
    }
}

/// The menu the row's own view attached: one command of the view's own.
struct ViewAttachedMenuSource;

impl Source for ViewAttachedMenuSource {
    fn key(&self) -> &str {
        "attached-menu"
    }

    fn prop(&mut self, _name: &str) -> Option<Value> {
        None
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _part: &str) -> Vec<Element> {
        Vec::new()
    }

    fn part_kinds(&mut self, part: &str) -> Vec<Option<String>> {
        match part {
            "items" => vec![Some(MenuItemProps::NAME.to_owned())],
            _ => Vec::new(),
        }
    }

    fn components(
        &mut self,
        part: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        if part != "items" {
            return Ok(0);
        }
        read(&mut ViewMenuItemSource {
            label: "Open as page",
        })?;
        Ok(1)
    }
}

/// One command of a menu, as the app describes it.
struct ViewMenuItemSource<'a> {
    label: &'a str,
}

impl Source for ViewMenuItemSource<'_> {
    fn key(&self) -> &str {
        self.label
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        (name == "label").then(|| Value::Text(self.label.to_owned()))
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _part: &str) -> Vec<Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        _part: &str,
        _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }
}

/// The row of one item, as the app describes it: a list row led by its checkbox.
struct ViewRowSource<'a> {
    key: &'a str,
}

impl Source for ViewRowSource<'_> {
    fn key(&self) -> &str {
        self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        match name {
            "title" => Some(Value::Text(format!("{}'s words", self.key))),
            _ => None,
        }
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, name: &str) -> Option<BoundValue> {
        (name == "checkbox").then(|| BoundValue::Bool(tilo_ui_base::component::Bound::default()))
    }

    fn children(&mut self, _part: &str) -> Vec<Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        _part: &str,
        _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }
}

/// What an item holds, shown where its row stands: the item's own line, and what is written
/// about it under that line. Its line leads with the width a row's leading control takes and
/// the gap after it, so its title stands where the rows beside it start theirs.
fn document(key: &str, lead: f32, gap: f32) -> Element {
    Box::new(
        Stack::stack()
            .fill_width()
            .gap(Rhythm::Related)
            .with_id(format!("document-{key}"))
            .push(Box::new(
                Row::row()
                    .fill_width()
                    .gap(Rhythm::None)
                    .align(Align::Center)
                    .push(Box::new(
                        Spacer::fixed(lead).with_id(format!("leading-{key}")),
                    ))
                    .push(Box::new(Spacer::fixed(gap)))
                    .push(Box::new(
                        Text::new(&format!("{}'s words", key)).with_id(format!("title-{key}")),
                    )),
            ))
            .push(Box::new(Text::new("what is written about it"))),
    )
}

/// The row of one item, as a view of the app's own: a list row under what the view writes
/// about it, so the row is no one component the list knows and is built as the view it is.
fn view_row(key: &str) -> Element {
    Box::new(
        Stack::stack()
            .fill_width()
            .gap(Rhythm::Tight)
            .with_id(format!("view-{key}"))
            .push(Box::new(ListRow::new(key, format!("{key}'s words"))))
            .push(Box::new(Text::new("under it"))),
    )
}

/// An item the person is done with reads as done: its row's title struck through, whether
/// the row is a bare list row or a view holding one with more under it.
#[test]
fn a_done_item_reads_struck_through_in_either_kind_of_row() {
    fn first_row(widget: &mut dyn Widget) -> Option<&mut ListRow> {
        if widget.as_any().is_some_and(|any| any.is::<ListRow>()) {
            return widget.as_any_mut()?.downcast_mut::<ListRow>();
        }
        widget.children_mut().into_iter().find_map(first_row)
    }
    let row = |done| ListItemProps {
        key: "bare".into(),
        done,
        row: ItemRow::Row(Box::new(ListRowProps {
            title: "Bump the version".into(),
            ..ListRowProps::default()
        })),
        ..ListItemProps::default()
    };
    for done in [false, true] {
        let rows = DescribedRows::given(vec![row(done)]);
        let (mut built, _, _) = bare_row(
            &rows,
            "bare",
            tilo_ui_base::motion::Feel::Standard,
            None,
            RowBuild::default(),
        )
        .expect("the item has a row");
        let struck = first_row(built.as_mut()).expect("a list row").is_struck();
        assert_eq!(struck, done, "a list row of an item done: {done}");
    }
    // A view holding the row with more under it: the row in it reads struck.
    let mut view: Element = Box::new(
        Stack::stack()
            .push(Box::new(ListRow::new("view-row", "Bump the version")))
            .push(Box::new(Text::new("under it"))),
    );
    assert!(strike_row(view.as_mut()), "the view holds a row to strike");
    assert!(first_row(view.as_mut()).expect("a list row").is_struck());
}

/// A keyed row answers its key however the app draws it: the item's key runs on the row the
/// list's keyboard is on, whether the row is one list row or a view of any kind, and the
/// window shows that row answered it.
#[test]
fn a_keyed_row_answers_whichever_way_it_is_drawn() {
    for views in [false, true] {
        let queue = tilo_ui_base::Intents::<Fired>::default();
        let mut rows = ViewRows::new(&[("row-0", None), ("row-1", None)], 20.0, 8.0)
            .keys(&["s"], Event::View(Intent::new(&queue, "row", "key")));
        if views {
            rows = rows.with_views();
        }
        let rendered: SharedRows = Rc::new(RefCell::new(rows));
        let mut list = DescribedList::from_props(
            "keys",
            VirtualListProps {
                rows: DescribedRows::test_holding(Held::Rendered(rendered)),
                ..VirtualListProps::default()
            },
        );
        let pressed = tilo_ui_base::synthetic_input::char_key("s");
        let handled = {
            let inner = list.list_mut().expect("the list stands");
            let (taffy, node) = lay_out_tree(inner, 400.0);
            Widget::set_focus(inner, true);
            inner.focus_row("row-0".to_owned());
            tilo_ui_app::testing::Dispatch::new()
                .layout(&taffy, node)
                .send(inner, &pressed)
                .handled
        };
        assert!(handled, "the row's key answers (a view: {views})");
        assert_eq!(
            queue.take(),
            [Fired {
                key: "row".to_owned(),
                event: "key".to_owned(),
                value: Some(Value::Text("s".to_owned())),
                gesture: None,
            }],
            "the item's `shortcut` reports the key pressed (a view: {views})"
        );
        let list = list.list().expect("the list stands");
        assert_eq!(
            list.focused_key().map(String::as_str),
            Some("row-0"),
            "the window shows the row the key acted on, under its keyboard ring"
        );
        assert!(
            list.row_rect(&"row-0".to_owned()).is_some(),
            "the row it answered on is the one on screen (a view: {views})"
        );
    }
}
