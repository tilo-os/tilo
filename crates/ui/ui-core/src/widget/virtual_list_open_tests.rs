//! The row a list opens where it stands: into the item's own document, one row at a time, and
//! only while the item is still a row of the list.
use super::test_support::*;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::test_support::{ProbeRow, lay_out};
use super::*;
use tilo_ui_base::collection_transition::ListModelChange;

use crate::testing::Dispatch;

tilo_test_env::scenario! {
    fn an_item_opens_closes_and_tells_its_host;

    phase the_host_hears_the_item_the_person_opened_and_told_when_it_closed;
    {
        let (mut list, opens, closes) = heard_list();
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);

        list.activate_key(ListKey::new(0u64));
        assert_eq!(*opens.borrow(), vec![0], "the host hears the item open");
        assert_eq!(closes.get(), 0);

        list.activate_key(ListKey::new(0u64));
        assert_eq!(list.open_row(), None);
        assert_eq!(closes.get(), 1);

        list.activate_key(ListKey::new(2u64));
        assert_eq!(*opens.borrow(), vec![0, 2]);
        assert!(
            Dispatch::new()
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &tilo_ui_base::synthetic_input::named_key(NamedKey::Escape)
                )
                .handled
        );
        assert_eq!(closes.get(), 2, "Escape closes the item, heard once");
    }

    phase an_item_a_host_holds_open_stands_open_where_it_stands;
    {
        let (mut list, _opens, _closes) = heard_list();
        let _ = lay_out(&mut list, 300.0);
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

    phase a_row_with_a_document_opens_where_it_stands_and_the_host_hears_nothing;
    {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let mut list = list(Rc::clone(&opened));
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);

        list.activate_key(ListKey::new(0u64));
        assert_eq!(list.open_row(), Some(&0), "the row opened where it stands");
        assert!(
            opened.borrow().is_empty(),
            "a row that opens in the list opens nothing else"
        );

        // A row without a document opens elsewhere: it is not the list's to open.
        list.activate_key(ListKey::new(1u64));
        assert_eq!(*opened.borrow(), vec![1]);
        assert_eq!(
            list.open_row(),
            Some(&0),
            "a row that opens elsewhere leaves the open one open"
        );

        list.activate_key(ListKey::new(2u64));
        assert_eq!(list.open_row(), Some(&2));

        assert!(
            Dispatch::new()
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &tilo_ui_base::synthetic_input::named_key(NamedKey::Escape)
                )
                .handled
        );
        assert_eq!(list.open_row(), None);
    }

    phase an_item_that_leaves_the_list_closes_with_it;
    {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let mut list = list(Rc::clone(&opened));
        let _ = lay_out(&mut list, 300.0);
        list.activate_key(ListKey::new(2u64));
        assert_eq!(list.open_row(), Some(&2));

        // The place switched, and the item the person was reading is not a row of it: the
        // document goes with the row it stood on.
        list.update_model(|model| {
            model.rows.retain(|row| *row != 2);
        });
        assert_eq!(list.open_row(), None);
    }


    phase a_row_named_again_keeps_held_open_while_the_rows_come_back;
    {
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

        rows.borrow_mut().extend(0..6_u64);
        list.notify_model_changes(&[ListModelChange::RowsChanged {
            keys: (0..6_u64).collect(),
        }]);
        assert_eq!(list.open_row(), Some(&2));
        assert_eq!(closes.get(), 0);
    }

    phase a_row_named_again_closes_when_the_rows_arrive_without_it;
    {
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

    phase a_list_too_narrow_to_hold_its_item_opens_it_in_front_instead;
    {
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
        narrow.activate_key(ListKey::new(0u64));
        assert_eq!(narrow.open_row(), None);
        assert_eq!(
            *opened.borrow(),
            vec![0],
            "the press opens the item in front, where the host shows it"
        );
        let opened = Rc::new(RefCell::new(Vec::new()));
        let mut reserved = list(Rc::clone(&opened));
        let _ = lay_out(&mut reserved, 300.0);
        reserved.activate_key(ListKey::new(0u64));
        assert_eq!(reserved.open_row(), Some(&0));
        assert!(opened.borrow().is_empty());
    }

    phase the_row_held_open_is_handed_to_a_reader_once;
    {
        let mut list = opening_list();
        let _ = lay_out(&mut list, 300.0);
        list.set_open(Some(0));

        let semantics = list.accessibility().expect("the list has a node");
        let item = semantics.children.first().expect("the list hands its rows");
        assert_eq!(item.role, tilo_ui_base::a11y::Role::ListItem);
        assert_eq!(item.state.expanded, Some(true));
        assert_eq!(item.name.as_deref(), Some("row 0"));
        let nested: Vec<_> = item
            .children
            .iter()
            .filter(|node| {
                matches!(
                    node.role,
                    tilo_ui_base::a11y::Role::ListItem | tilo_ui_base::a11y::Role::TreeItem
                )
            })
            .collect();
        assert!(
            nested.is_empty(),
            "the open row is one row, not one inside another: {nested:?}"
        );
    }
}

/// A row a host holds open below the fold is scrolled into view, however long the list: a task
/// a search found or an agent opened shows its page inside the window, rather than growing it
/// past the window's foot where what a person types into it goes nowhere.
#[test]
fn a_row_held_open_below_the_fold_is_scrolled_into_view_at_ten_thousand_rows() {
    let mut list = VirtualList::with_selection_delegate(
        Opens {
            rows: (0..10_000_u64).collect(),
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
    list.set_key_presenting(|item: &str| item.parse().ok());
    let _ = lay_out(&mut list, 300.0);
    assert!(
        !list.visible_range().contains(&9_998),
        "the row starts far below the fold"
    );

    list.hold_open(Some("9998"));
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        list.open_row(),
        Some(&9_998),
        "the row opens where it stands"
    );
    assert!(
        list.visible_range().contains(&9_998),
        "and the window brings it up: {:?}",
        list.visible_range()
    );
}
