//! The rows a list mounts as a scroll carries on, and as it jumps.

use std::cell::Cell;
use std::rc::Rc;

use super::test_support::{HugeList, ProbeRow, lay_out};
use super::*;

tilo_test_env::scenario! {
    fn a_list_mounts_only_what_its_window_needs;

    phase a_reveal_brings_a_row_below_the_fold_into_view_without_choosing_it;
    {
        // Ten thousand rows of 20 px in a 200 px view: an address book's letter rail asks the
        // list to show the first card under a letter, which stands far below the fold.
        let model = VecListModel::new((0..10_000_u64).collect(), |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(200.0);
        list.set_key_presenting(|item: &str| item.parse().ok());
        let _ = lay_out(&mut list, 300.0);
        assert!(
            !list.visible_range().contains(&9_000),
            "the row starts far below the fold"
        );
        // The view asks for it: the list moves to its row, at the list's own motion.
        list.scroll_reveal("9000");
        for _ in 0..4 {
            let _ = lay_out(&mut list, 300.0);
        }
        let shown = list
            .mounted
            .iter()
            .map(|row| row.key.typed::<u64>())
            .collect::<Vec<_>>();
        assert!(
            shown.contains(&9_000),
            "the row the view asked for is in the window: {shown:?}"
        );
        // It is shown, not chosen: a view asking the list to show a row is not a person picking
        // one.
        assert_eq!(list.selection_len(), 0, "a revealed row is not chosen");
        // A row above the fold brings the list back up.
        list.scroll_reveal("3");
        for _ in 0..4 {
            let _ = lay_out(&mut list, 300.0);
        }
        let shown = list
            .mounted
            .iter()
            .map(|row| row.key.typed::<u64>())
            .collect::<Vec<_>>();
        assert!(shown.contains(&3), "the row above is in the window: {shown:?}");

        // The list tells the view the row it brought up, and the view takes the ask back on it:
        // a person who scrolled away presses the same letter again, and it works.
        let told: Rc<std::cell::RefCell<Vec<u64>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
        let told_by = Rc::clone(&told);
        list.set_on_revealed(move |key| told_by.borrow_mut().push(key));
        list.scroll_reveal("9000");
        for _ in 0..4 {
            let _ = lay_out(&mut list, 300.0);
        }
        assert_eq!(*told.borrow(), vec![9_000], "the list says which row it brought up");
        // The person scrolls away: the list stays where they left it.
        list.set_scroll_offset(0.0);
        let _ = lay_out(&mut list, 300.0);
        assert!(
            !list.visible_range().contains(&9_000),
            "the person scrolled away from it"
        );
        // The same ask again brings it up again, because the view took the last one back.
        list.scroll_reveal("9000");
        for _ in 0..4 {
            let _ = lay_out(&mut list, 300.0);
        }
        let shown = list
            .mounted
            .iter()
            .map(|row| row.key.typed::<u64>())
            .collect::<Vec<_>>();
        assert!(
            shown.contains(&9_000),
            "the same row comes up again: {shown:?}"
        );
        assert_eq!(
            *told.borrow(),
            vec![9_000, 9_000],
            "the view heard both asks, so it cleared between them"
        );
    }

    phase a_scroll_that_carries_on_mounts_rows_ahead_and_a_jump_only_what_it_shows;
    {
        // Rows of 20 px in a 200 px view: an overscan of 40 px, and a view (200 px) mounted ahead
        // of a scroll, so a notch of a wheel lands among the rows already mounted.
        let model = VecListModel::new((0..1000_u64).collect(), |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(200.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(
            list.mounted_range,
            list.visible_range(),
            "at rest: what it shows"
        );
        let keys = |list: &VirtualList<VecListModel<u64, u64>>| {
            list.mounted.iter().map(|row| row.key.typed::<u64>()).collect::<Vec<_>>()
        };

        list.set_scroll_offset(20.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(
            list.mounted_range,
            0..24,
            "the rows a view ahead come with the first"
        );
        let mounted = keys(&list);

        list.set_scroll_offset(60.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(keys(&list), mounted);

        list.set_scroll_offset(400.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.mounted_range, 8..43);
        assert_eq!(
            list.mounted.first().map(|row| row.key.clone()),
            Some(ListKey::new(8u64)),
            "rows more than a view behind let go"
        );

        // A wheel turned back by a notch stands on the rows a view behind: nothing new mounts.
        let mounted = keys(&list);
        list.set_scroll_offset(340.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(keys(&list), mounted, "a scroll turned back mounts nothing");

        list.set_scroll_offset(2_000.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(
            list.mounted_range,
            list.visible_range(),
            "a jump: what it shows"
        );
        assert_eq!(list.mounted.len(), list.visible_range().len());

        list.set_scroll_offset(1_980.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.mounted_range, 87..122);
    }

    phase hundred_million_row_lists_mount_and_request_only_the_window;
    {
        {
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let presses = Rc::new(Cell::new(0));
            let mut list = VirtualList::new(HugeList { count: 100_000_000 }, 20.0, move |_, _, key| {
                captured.set(captured.get().saturating_add(1));
                Box::new(ProbeRow::new(
                    format!("row {key}"),
                    20.0,
                    false,
                    Rc::clone(&presses),
                ))
            })
            .with_viewport_height(100.0);
            list.set_scroll_offset(50_000_000.0 * 20.0);
            let _ = lay_out(&mut list, 300.0);

            assert_eq!(list.model().len(), 100_000_000);
            assert!(list.mounted_len() <= 10);
            assert_eq!(builds.get(), list.mounted_len());
            assert!(list.visible_range().len() <= 10);
            let top = list.scrollbar_thumb().expect("overflow has a scrollbar");
            let offset = list.max_scroll_offset();
            list.set_scroll_offset(offset);
            let bottom = list.scrollbar_thumb().expect("scrollbar remains visible");
            assert!(bottom.y0 > top.y0);
            let track = crate::widget::scrollbar::vertical_track(list.rect, list.gutter);
            assert!((bottom.y1 - track.y1).abs() < 0.001);
        }
    }

}
