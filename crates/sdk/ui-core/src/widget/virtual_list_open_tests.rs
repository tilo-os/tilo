//! The row a list opens where it stands: into the item's own document, one row at a time, and
//! only while the item is still a row of the list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::tests::{ProbeRow, lay_out};
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
