//! The rows a list mounts as a scroll carries on, and as it jumps.

use std::cell::Cell;
use std::rc::Rc;

use super::tests::{HugeList, ProbeRow, lay_out};
use super::*;

#[test]
fn a_scroll_that_carries_on_mounts_rows_ahead_and_a_jump_only_what_it_shows() {
    // Rows of 20 px in a 200 px view: an overscan of 40 px, and a
    // quarter of the view (50 px) mounted ahead of a scroll.
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
        list.mounted.iter().map(|row| row.key).collect::<Vec<_>>()
    };

    list.set_scroll_offset(20.0);
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        list.mounted_range,
        0..16,
        "the rows ahead come with the first"
    );
    let mounted = keys(&list);

    // Two rows further the view and its overscan stay within the rows
    // mounted: nothing mounts.
    list.set_scroll_offset(60.0);
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(keys(&list), mounted);

    list.set_scroll_offset(100.0);
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(list.mounted_range, 3..20);
    assert_eq!(
        list.mounted.first().map(|row| row.key),
        Some(3),
        "rows behind let go"
    );

    list.set_scroll_offset(2_000.0);
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        list.mounted_range,
        list.visible_range(),
        "a jump: what it shows"
    );
    assert_eq!(list.mounted.len(), list.visible_range().len());

    // Back up: the rows ahead are above.
    list.set_scroll_offset(1_980.0);
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(list.mounted_range, 94..112);
}

#[test]
fn a_changed_row_is_mounted_again_in_place_and_alone() {
    let built = Rc::new(Cell::new(0_u32));
    let counted = Rc::clone(&built);
    let model = VecListModel::new((0..10_u64).collect(), |key| *key).unwrap();
    let mut list = VirtualList::new(model, 20.0, move |_, _, key| {
        counted.set(counted.get() + 1);
        Box::new(ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(200.0);
    let _ = lay_out(&mut list, 300.0);
    let _ = crate::widget::Widget::take_structure_change(&mut list);
    let before = built.get();

    list.invalidate_rows([3]);
    // The host syncs before it asks what changed.
    let doc = automerge::AutoCommit::new();
    crate::widget::Widget::sync(&mut list, &doc, &automerge::ROOT);
    assert_eq!(
        crate::widget::Widget::take_structure_change(&mut list),
        crate::widget::StructureChange::Children,
        "a row whose content changed is mounted again in place: the window keeps its tree"
    );
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        built.get() - before,
        1,
        "only the changed row is built again"
    );
    assert!(list.mounted.iter().any(|row| row.key == 3));
}

#[test]
fn a_row_the_model_says_changed_is_mounted_again_in_place_and_alone() {
    let built = Rc::new(Cell::new(0_u32));
    let counted = Rc::clone(&built);
    let model = VecListModel::new((0..10_u64).collect(), |key| *key).unwrap();
    let mut list = VirtualList::new(model, 20.0, move |_, _, key| {
        counted.set(counted.get() + 1);
        Box::new(ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(200.0);
    let _ = lay_out(&mut list, 300.0);
    let _ = crate::widget::Widget::take_structure_change(&mut list);
    let before = built.get();

    // As a model that renders its rows reports one it changed (a described list's rows).
    list.notify_model_changes(&[ListModelChange::RowsChanged { keys: vec![3] }]);
    let doc = automerge::AutoCommit::new();
    crate::widget::Widget::sync(&mut list, &doc, &automerge::ROOT);
    assert_eq!(
        crate::widget::Widget::take_structure_change(&mut list),
        crate::widget::StructureChange::Children,
        "a row the model changed is mounted again in place: the window keeps its tree"
    );
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(
        built.get() - before,
        1,
        "only the changed row is built again"
    );
    assert!(list.mounted.iter().any(|row| row.key == 3));
}

#[test]
fn hundred_million_row_lists_mount_and_request_only_the_window() {
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
        list.set_scroll_offset(list.max_scroll_offset());
        let bottom = list.scrollbar_thumb().expect("scrollbar remains visible");
        assert!(bottom.y0 > top.y0);
        let track = crate::widget::scrollbar::vertical_track(list.rect, list.gutter);
        assert!((bottom.y1 - track.y1).abs() < 0.001);
    }
}
