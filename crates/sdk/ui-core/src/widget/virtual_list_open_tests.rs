//! The row a list opens where it stands: into the item's own document, one row at a time, and
//! only while the item is still a row of the list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::tests::{ProbeRow, inside, lay_out, named, within};
use super::*;

use crate::testing::Dispatch;

/// A model whose even rows carry a document of their own: they open where they stand.
struct Opens {
    rows: Vec<u64>,
}

impl ListModel for Opens {
    type Key = u64;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.rows.get(index).copied()
    }

    fn index_of(&self, key: &u64) -> Option<usize> {
        self.rows.iter().position(|row| row == key)
    }

    fn opens_in_place(&self, index: usize) -> bool {
        index.is_multiple_of(2)
    }
}

/// A closed row of 20 px that grows to 120 px while it stands open, and reports what it is told.
fn list(opened: Rc<RefCell<Vec<u64>>>) -> VirtualList<Opens> {
    VirtualList::with_selection_delegate(
        Opens {
            rows: (0..6_u64).collect(),
        },
        20.0,
        move |_, _, key, build| {
            let height = if build.expanded { 120.0 } else { 20.0 };
            Box::new(ProbeRow::new(
                format!("row {key}"),
                height,
                false,
                Rc::new(Cell::new(0)),
            )) as Element
        },
    )
    .with_viewport_height(200.0)
    .on_activate(move |key| opened.borrow_mut().push(key))
}

/// The list with what the host hears of the item standing open: the rows it opened and the
/// count of times one closed.
fn heard_list() -> (VirtualList<Opens>, Rc<RefCell<Vec<u64>>>, Rc<Cell<usize>>) {
    let opens: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let closes = Rc::new(Cell::new(0));
    let list = VirtualList::with_selection_delegate(
        Opens {
            rows: (0..6_u64).collect(),
        },
        20.0,
        |_, _, key, build| {
            let height = if build.expanded { 120.0 } else { 20.0 };
            Box::new(ProbeRow::new(
                format!("row {key}"),
                height,
                false,
                Rc::new(Cell::new(0)),
            )) as Element
        },
    )
    .with_viewport_height(200.0);
    let (heard, shut) = (Rc::clone(&opens), Rc::clone(&closes));
    let list = list
        .on_opened(move |key| heard.borrow_mut().push(key))
        .on_closed(move || shut.set(shut.get() + 1));
    (list, opens, closes)
}

#[test]
fn the_host_hears_the_item_the_person_opened_and_told_when_it_closed() {
    let (mut list, opens, closes) = heard_list();
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);

    list.activate_key(0);
    assert_eq!(*opens.borrow(), vec![0], "the host hears the item open");
    assert_eq!(closes.get(), 0);

    // Pressing the row standing open closes it, and the host hears that too.
    list.activate_key(0);
    assert_eq!(list.open_row(), None);
    assert_eq!(closes.get(), 1);

    // A row opened from the host shows the same: what the person opened, the list keeps.
    list.activate_key(2);
    assert_eq!(*opens.borrow(), vec![0, 2]);
    // Escape closes it, heard once.
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(
                &mut list,
                &crate::synthetic_input::named_key(NamedKey::Escape)
            )
            .handled
    );
    assert_eq!(closes.get(), 2, "Escape closes the item, heard once");
}

#[test]
fn an_item_a_host_holds_open_stands_open_where_it_stands() {
    let (mut list, _opens, _closes) = heard_list();
    let _ = lay_out(&mut list, 300.0);
    // What a host keeps and gives back: the item it holds open is the row standing open, and a
    // row without a document it names opens nowhere, so no row stands open.
    list.set_open(Some(4));
    assert_eq!(list.open_row(), Some(&4));
    list.set_open(Some(1));
    assert_eq!(
        list.open_row(),
        None,
        "a row without a document opens nowhere"
    );
    list.set_open(Some(4));
    assert_eq!(list.open_row(), Some(&4), "and it opens again when named");
    list.set_open(None);
    assert_eq!(list.open_row(), None);
}

#[test]
fn a_row_with_a_document_opens_where_it_stands_and_the_host_hears_nothing() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut list = list(Rc::clone(&opened));
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);

    list.activate_key(0);
    assert_eq!(list.open_row(), Some(&0), "the row opened where it stands");
    assert!(
        opened.borrow().is_empty(),
        "a row that opens in the list opens nothing else"
    );

    // A row without a document opens elsewhere: it is not the list's to open.
    list.activate_key(1);
    assert_eq!(*opened.borrow(), vec![1]);
    assert_eq!(
        list.open_row(),
        Some(&0),
        "a row that opens elsewhere leaves the open one open"
    );

    // One row at a time: the next one to open closes the one before it.
    list.activate_key(2);
    assert_eq!(list.open_row(), Some(&2));

    // Escape closes the document and leaves the list where it was.
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(
                &mut list,
                &crate::synthetic_input::named_key(NamedKey::Escape)
            )
            .handled
    );
    assert_eq!(list.open_row(), None);
}

#[test]
fn an_item_that_leaves_the_list_closes_with_it() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut list = list(Rc::clone(&opened));
    let _ = lay_out(&mut list, 300.0);
    list.activate_key(2);
    assert_eq!(list.open_row(), Some(&2));

    // The place switched, and the item the person was reading is not a row of it: the
    // document goes with the row it stood on.
    list.update_model(|model| {
        model.rows.retain(|row| *row != 2);
    });
    assert_eq!(list.open_row(), None);
}

#[test]
fn an_item_that_leaves_the_list_is_heard_to_close() {
    let (mut list, _opens, closes) = heard_list();
    let _ = lay_out(&mut list, 300.0);
    list.activate_key(2);
    assert_eq!(closes.get(), 0);
    list.update_model(|model| {
        model.rows.retain(|row| *row != 2);
    });
    assert_eq!(
        closes.get(),
        1,
        "the host hears the item close with its row"
    );
}

#[test]
fn the_row_that_opens_grows_into_its_document_and_keeps_the_list_laying_out() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut list = list(Rc::clone(&opened));
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        list.viewport.item_height(0),
        Some(20.0),
        "a closed row stands at its row"
    );

    // A press on the row opens it, and the row grows into its document on the frames after.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    list.activate_key(0);
    let _ = lay_out(&mut list, 300.0);
    let on_the_way = list.viewport.item_height(0).unwrap_or_default();
    assert!(
        on_the_way < 120.0,
        "the row is on its way to its document: {on_the_way}"
    );
    assert!(
        list.is_layout_dirty() && list.layout_needs_measure(),
        "the list asks for the frames its row moves in"
    );
}

/// A model whose app names its rows again as a whole, as it does when the list is read anew:
/// the keys go until it answers with the ones it reads now.
struct Renamed {
    rows: Rc<RefCell<Vec<u64>>>,
}

impl ListModel for Renamed {
    type Key = u64;

    fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.rows.borrow().get(index).copied()
    }

    fn index_of(&self, key: &u64) -> Option<usize> {
        self.rows.borrow().iter().position(|row| row == key)
    }

    fn opens_in_place(&self, index: usize) -> bool {
        index.is_multiple_of(2)
    }
}

/// A list over rows the test names again, and what the host hears of the open row.
fn renamed_list() -> (VirtualList<Renamed>, Rc<RefCell<Vec<u64>>>, Rc<Cell<usize>>) {
    let rows: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new((0..6_u64).collect()));
    let closes = Rc::new(Cell::new(0));
    let shut = Rc::clone(&closes);
    let list = VirtualList::with_selection_delegate(
        Renamed {
            rows: Rc::clone(&rows),
        },
        20.0,
        |_, _, key, build| {
            let height = if build.expanded { 120.0 } else { 20.0 };
            Box::new(ProbeRow::new(
                format!("row {key}"),
                height,
                false,
                Rc::new(Cell::new(0)),
            )) as Element
        },
    )
    .with_viewport_height(200.0)
    .on_closed(move || shut.set(shut.get() + 1));
    (list, rows, closes)
}

#[test]
fn a_row_named_again_keeps_standing_open_while_the_rows_come_back() {
    let (mut list, rows, closes) = renamed_list();
    let _ = lay_out(&mut list, 300.0);
    list.set_open(Some(2));
    assert_eq!(list.open_row(), Some(&2));

    // The app names the list's rows again: the keys go until it answers with the ones it reads
    // now, so the row that stood open is not one that left.
    rows.borrow_mut().clear();
    list.notify_model_changes(&[ListModelChange::Reset]);
    assert_eq!(
        list.open_row(),
        Some(&2),
        "the open waits for the rows the app names"
    );
    assert_eq!(closes.get(), 0, "a row named again is not heard to close");

    // They arrive with the row where it stood, under the key it had: it goes on standing open.
    rows.borrow_mut().extend(0..6_u64);
    list.notify_model_changes(&[ListModelChange::RowsChanged {
        keys: (0..6_u64).collect(),
    }]);
    assert_eq!(list.open_row(), Some(&2));
    assert_eq!(closes.get(), 0);
}

#[test]
fn a_row_named_again_closes_when_the_rows_arrive_without_it() {
    let (mut list, rows, closes) = renamed_list();
    let _ = lay_out(&mut list, 300.0);
    list.set_open(Some(2));

    rows.borrow_mut().clear();
    list.notify_model_changes(&[ListModelChange::Reset]);
    assert_eq!(list.open_row(), Some(&2));

    // The rows the app reads now do not hold the item: the open closes with the row it stood
    // on, and the host hears it.
    rows.borrow_mut().extend([0_u64, 1, 3, 4, 5]);
    list.notify_model_changes(&[ListModelChange::RowsChanged {
        keys: vec![0, 1, 3, 4, 5],
    }]);
    assert_eq!(list.open_row(), None);
    assert_eq!(closes.get(), 1);
}

#[test]
fn a_list_too_narrow_to_hold_its_item_opens_it_in_front_instead() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut narrow = list(Rc::clone(&opened)).in_place(false);
    let _ = lay_out(&mut narrow, 300.0);
    // What a host holds open as the window narrows, and what a press does at that width: the
    // item opens in front of the list, through the host, never grown into the row.
    narrow.set_open(Some(0));
    assert_eq!(
        narrow.open_row(),
        None,
        "a list too narrow for the item keeps no row open"
    );
    narrow.activate_key(0);
    assert_eq!(narrow.open_row(), None);
    assert_eq!(
        *opened.borrow(),
        vec![0],
        "the press opens the item in front, where the host shows it"
    );
    // A list with the room to hold the item opens it where it stands.
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut roomy = list(Rc::clone(&opened));
    let _ = lay_out(&mut roomy, 300.0);
    roomy.activate_key(0);
    assert_eq!(roomy.open_row(), Some(&0));
    assert!(opened.borrow().is_empty());
}

/// A list whose even rows carry a document, drawn as the row standing open into it draws it.
fn opening_list() -> VirtualList<Opens> {
    VirtualList::with_selection_delegate(
        Opens {
            rows: (0..6_u64).collect(),
        },
        20.0,
        |_, _, key, build| {
            let document: Element = Box::new(ProbeRow::new(
                format!("row {key}"),
                120.0,
                false,
                Rc::new(Cell::new(0)),
            ));
            if build.expanded {
                Box::new(super::open::RowOpen::new(document, false)) as Element
            } else {
                document
            }
        },
    )
    .with_viewport_height(200.0)
}

/// The row standing open into its item is the item, and a reader is handed it once: the box that
/// took the row's place names the same row, so the item's controls stand under the row rather
/// than under a second row of the same item.
#[test]
fn the_row_standing_open_is_handed_to_a_reader_once() {
    let mut list = opening_list();
    let _ = lay_out(&mut list, 300.0);
    list.set_open(Some(0));

    let semantics = list.accessibility().expect("the list has a node");
    let item = semantics.children.first().expect("the list hands its rows");
    assert_eq!(item.role, crate::a11y::Role::ListItem);
    assert_eq!(item.state.expanded, Some(true));
    assert_eq!(item.name.as_deref(), Some("row 0"));
    let nested: Vec<_> = item
        .children
        .iter()
        .filter(|node| {
            matches!(
                node.role,
                crate::a11y::Role::ListItem | crate::a11y::Role::TreeItem
            )
        })
        .collect();
    assert!(
        nested.is_empty(),
        "the open row is one row, not one inside another: {nested:?}"
    );
}

/// The document an item opens into where its row stands: the field its title is typed in, the
/// page under it, the box that ticks it off and the button that saves it.
fn document(saved: Rc<Cell<usize>>, done: crate::LocalState<bool>) -> Element {
    use crate::widget::{Button, Checkbox, Column, TextInput};
    Box::new(
        Column::new()
            .push(Box::new(
                TextInput::new(Vec::new())
                    .with_text_role(crate::tokens::TextRole::Title)
                    .with_id("title")
                    .with_accessible_name("Title")
                    .with_initial_value("Ring the plumber"),
            ))
            .push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("notes")
                    .multiline(true)
                    .with_accessible_name("Notes")
                    .with_initial_value("The tap drips"),
            ))
            .push(Box::new(
                Checkbox::from_state(&done).with_accessible_name("Done"),
            ))
            .push(Box::new(
                Button::new("save", "Save").on_view(move || saved.set(saved.get() + 1)),
            )),
    )
}

/// A list whose even rows open the item into a document of its own where they stand.
fn documented_list(saved: Rc<Cell<usize>>, done: crate::LocalState<bool>) -> VirtualList<Opens> {
    VirtualList::with_selection_delegate(
        Opens {
            rows: (0..6_u64).collect(),
        },
        20.0,
        move |_, _, key, build| {
            if build.expanded {
                Box::new(super::open::RowOpen::new(
                    document(Rc::clone(&saved), done.clone()),
                    false,
                )) as Element
            } else {
                Box::new(ProbeRow::new(
                    format!("row {key}"),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                )) as Element
            }
        },
    )
    .with_viewport_height(400.0)
}

/// An item open where its row stands hands a reader the fields that edit it: the title, the
/// page, the box that ticks it off and the buttons, each under the row it is in and where it is
/// drawn, and each operated as an assistive technology operates it.
#[test]
fn an_item_open_in_its_row_hands_a_reader_the_fields_that_edit_it() {
    use crate::a11y::Action;
    use crate::headless::FrameHarness;
    use accesskit::{Action as AkAction, Role as AkRole};

    let saved = Rc::new(Cell::new(0));
    let done = crate::LocalState::new(false);
    let mut harness = FrameHarness::for_root(
        crate::State::new(),
        documented_list(Rc::clone(&saved), done.clone()),
        (400, 600),
        crate::theme::Theme::dark(),
    );
    harness.root_mut::<VirtualList<Opens>>().set_open(Some(0));
    harness.relayout();

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Ring the plumber");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = inside(&nodes, row_id);
    assert_eq!(
        controls
            .iter()
            .filter(|(_, node)| matches!(node.role(), AkRole::ListItem | AkRole::TreeItem))
            .count(),
        0,
        "the open row is one row, not one inside another"
    );
    let (_, title) = named(&controls, AkRole::TextInput, "Title");
    assert_eq!(
        title.value(),
        Some("Ring the plumber"),
        "the field reads the title it edits"
    );
    let (_, page) = named(&controls, AkRole::MultilineTextInput, "Notes");
    assert_eq!(page.value(), Some("The tap drips"));
    let (checkbox, box_node) = named(&controls, AkRole::CheckBox, "Done");
    assert_eq!(box_node.toggled(), Some(accesskit::Toggled::False));
    assert!(
        box_node.supports_action(AkAction::Click),
        "the box can be ticked from a reader"
    );
    named(&controls, AkRole::Button, "Save");
    for (_, control) in &controls {
        let bounds = control
            .bounds()
            .unwrap_or_else(|| panic!("{:?} is drawn nowhere", control.role()));
        assert!(
            within(row_bounds, bounds),
            "{:?} stands outside its row: {bounds:?} not in {row_bounds:?}",
            control.role()
        );
    }

    // The keyboard reaches the same controls, in the same order a reader is handed them.
    let stops: Vec<(crate::a11y::Role, Option<String>)> = harness
        .tab_through()
        .into_iter()
        .filter(|(role, _)| {
            matches!(
                role,
                crate::a11y::Role::TextInput
                    | crate::a11y::Role::MultilineTextInput
                    | crate::a11y::Role::Checkbox
                    | crate::a11y::Role::Button
            )
        })
        .collect();
    let named_stops: Vec<Option<&str>> = stops
        .iter()
        .map(|(_, name)| name.as_deref())
        .filter(|name| matches!(*name, Some("Title" | "Notes" | "Done" | "Save")))
        .collect();
    assert_eq!(
        named_stops,
        vec![Some("Title"), Some("Notes"), Some("Done"), Some("Save")],
        "the keyboard reaches what the row holds, in reading order"
    );

    // The box ticks, and the row says so.
    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    let nodes = harness.a11y_nodes();
    let (row_id, _) = named(&nodes, AkRole::ListItem, "Ring the plumber");
    let controls = inside(&nodes, row_id);
    assert_eq!(
        named(&controls, AkRole::CheckBox, "Done").1.toggled(),
        Some(accesskit::Toggled::True),
        "the tick reaches the box"
    );
    assert!(done.with(|checked| *checked), "the box's value is ticked");

    // The button is pressed.
    let (button, _) = named(&nodes, AkRole::Button, "Save");
    harness
        .perform_a11y_action(button, &Action::Activate)
        .expect("the button is pressed");
    assert_eq!(saved.get(), 1, "the press reaches the button");

    // The page is written to.
    let nodes = harness.a11y_nodes();
    let (page, _) = named(&nodes, AkRole::MultilineTextInput, "Notes");
    harness
        .perform_a11y_action(page, &Action::SetValue("The tap runs".into()))
        .expect("the page is written in");
    let nodes = harness.a11y_nodes();
    assert_eq!(
        named(&nodes, AkRole::MultilineTextInput, "Notes").1.value(),
        Some("The tap runs"),
        "what the reader writes stands in the page"
    );
}
