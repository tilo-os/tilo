//! The rows a list mounts as a scroll carries on, and as it jumps.

use std::cell::Cell;
use std::rc::Rc;

use super::tests::{ProbeRow, lay_out};
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
