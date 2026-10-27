//! A sort clicked on a header and rows the person adds or removes move on
//! the standard list change (`motion::ListChange`): only rows shown in the
//! body move, rows coming from off-screen fade in at their slot, and a key,
//! reduced motion and a settled table land and ask for nothing.

use super::*;
use crate::time::Duration;
use crate::tokens::MotionSource;

#[derive(Clone)]
struct R {
    name: String,
}

fn rows(names: &[&str]) -> Vec<R> {
    names
        .iter()
        .map(|name| R {
            name: (*name).to_owned(),
        })
        .collect()
}

/// `first`, then every two-digit name from `00` to `99` not in it.
fn hundred(first: &[&str]) -> Vec<R> {
    let mut names: Vec<String> = first.iter().map(|name| (*name).to_owned()).collect();
    for i in 0..100 {
        let name = format!("{i:02}");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.into_iter().map(|name| R { name }).collect()
}

fn table(rows: Vec<R>) -> DataTable<R> {
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text(|row: &R| row.name.clone()),
    ])
    .with_row_key(|row: &R| row.name.parse::<u64>().expect("a two-digit name"));
    table.set_rows(rows);
    table.set_viewport(100.0, 24.0);
    table
}

fn key(name: &str) -> RowIdentity {
    RowIdentity {
        value: name.parse().expect("a two-digit name"),
        occurrence: 0,
    }
}

/// One frame: layout (only when something asked for it) and a draw.
fn frame(table: &mut DataTable<R>, theme: &Theme) {
    if Widget::is_layout_dirty(&*table) || table.rect == Rect::ZERO {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = table.build_layout(&mut tree, theme, &mut node_map);
        let root = tree.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size::from_lengths(300.0, 400.0),
                ..Default::default()
            },
            &[node],
        );
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(300.0),
                height: taffy::AvailableSpace::Definite(400.0),
            },
        )
        .expect("layout");
        table.apply_layout(&tree, node, Point::ZERO);
    }
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    table.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::default(),
    );
}

/// Lay out and draw until the table's layout rests.
fn settled_frames(table: &mut DataTable<R>, theme: &Theme) {
    for _ in 0..3 {
        frame(table, theme);
    }
}

#[test]
fn a_header_click_moves_the_shown_rows_to_their_new_places_and_nothing_else() {
    // The frame's instant is held, so the test reads the motion where it
    // starts.
    let _frame = crate::motion::pin_frame();
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);
    let before_03 = table.viewport.item_view_top(1).expect("shown");
    let before_01 = table.viewport.item_view_top(3).expect("shown");

    crate::motion::note_source(MotionSource::Pointer);
    assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
    frame(&mut table, &theme);
    let now = crate::motion::frame_now();

    let names: Vec<_> = table
        .view_rows()
        .iter()
        .take(4)
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(names, ["00", "01", "02", "03"], "the order lands at once");
    let after_03 = table.viewport.item_view_top(3).expect("shown");
    let after_01 = table.viewport.item_view_top(1).expect("shown");
    let moving_03 = table.moving.rows.get(&key("03")).expect("03 moves down");
    let moving_01 = table.moving.rows.get(&key("01")).expect("01 moves up");
    assert!(
        (moving_03.offset(now) - (before_03 - after_03)).abs() < 2.0,
        "03 is drawn where it stood, above its new place"
    );
    assert!(
        (moving_01.offset(now) - (before_01 - after_01)).abs() < 2.0,
        "01 is drawn where it stood, below its new place"
    );
    assert!(
        table.moving.rows.get(&key("00")).is_none() && table.moving.rows.get(&key("02")).is_none(),
        "rows the sort left in place do not move"
    );
    assert!(
        table.moving.rows.get(&key("50")).is_none(),
        "nothing outside the viewport animates"
    );
    assert!(Widget::needs_animation_frame(&table));
    assert!(Widget::needs_redraw(&table));
    // The rows move by transform: the table is laid out once, at the new
    // order, and its frames during the motion lay nothing out.
    assert!(!Widget::is_layout_dirty(&table));
    frame(&mut table, &theme);
    assert!(!Widget::is_layout_dirty(&table));

    // It settles exactly and asks for no more frames.
    let later = now + Duration::from_secs(2);
    assert!(!table.moving.rows.is_moving(later));
    table
        .moving
        .rows
        .start(std::iter::empty(), theme.motion(), later);
    assert!(
        table.moving.rows.is_empty(),
        "rows that arrived are forgotten"
    );
    let until = crate::motion::frames_until().expect("the sort asked for frames");
    assert!(
        crate::motion::frames_wanted(until),
        "the frame showing the rows at rest is still owed"
    );
    crate::motion::frame_drawn(until);
    assert!(!crate::motion::frames_wanted(until));
}

#[test]
fn rows_a_click_sort_brings_from_off_screen_fade_in_at_their_slot() {
    // The frame's instant is held, so the test reads the motion where it
    // starts.
    let _frame = crate::motion::pin_frame();
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "90", "91", "92", "93", "94"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Pointer);
    assert!(table.toggle_sort(0).is_some());
    frame(&mut table, &theme);
    let now = crate::motion::frame_now();

    let arriving = table
        .moving
        .rows
        .get(&key("01"))
        .expect("01 came from below");
    assert!(arriving.opacity(now) < 0.5, "it fades in");
    assert!(
        arriving.offset(now).abs() < f64::EPSILON,
        "at its own slot, without travelling across the table"
    );
    assert!(
        table.moving.rows.get(&key("90")).is_none(),
        "a row that left the viewport is not drawn on its way out"
    );
    assert!(table.moving.rows.get(&key("00")).is_none());
}

#[test]
fn a_sort_by_key_lands_on_the_next_frame() {
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Keyboard);
    assert!(table.toggle_sort(0).is_some());
    assert!(!table.moving.rows.has_pending(), "a key's result lands");
    frame(&mut table, &theme);
    assert!(table.moving.rows.is_empty());
    assert!(!table.moving.rows.is_moving(crate::time::now()));
}

#[test]
fn under_reduced_motion_a_clicked_sort_lands() {
    let theme = Theme::dark().with_reduce_motion(true);
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Pointer);
    assert!(table.toggle_sort(0).is_some());
    frame(&mut table, &theme);
    assert!(table.moving.rows.is_empty(), "rows land in place at once");
    assert!(!Widget::needs_animation_frame(&table));
}

#[test]
fn a_row_the_person_adds_fades_in_and_pushes_the_shown_rows_down() {
    // The frame's instant is held, so the test reads the motion where it
    // starts.
    let _frame = crate::motion::pin_frame();
    let theme = Theme::dark();
    let mut table = table(rows(&["00", "01", "02", "03", "04", "05", "06", "07"]));
    settled_frames(&mut table, &theme);
    let before_01 = table.viewport.item_view_top(1).expect("shown");

    crate::motion::note_source(MotionSource::Pointer);
    table
        .rows_mut()
        .insert(1, Box::new(R { name: "50".into() }));
    table.apply_changes(&[TableModelChange::RowsInserted { index: 1, count: 1 }]);
    frame(&mut table, &theme);
    let now = crate::motion::frame_now();

    let added = table.moving.rows.get(&key("50")).expect("the new row");
    assert!(added.opacity(now) < 0.5, "the added row fades in");
    let after_01 = table.viewport.item_view_top(2).expect("shown");
    let pushed = table.moving.rows.get(&key("01")).expect("01 moves down");
    assert!(
        (pushed.offset(now) - (before_01 - after_01)).abs() < 2.0,
        "the row below starts where it stood"
    );
    assert!(table.moving.rows.get(&key("00")).is_none());

    // The same change by a key lands.
    crate::motion::note_source(MotionSource::Keyboard);
    table.rows_mut().remove(1);
    table.apply_changes(&[TableModelChange::RowsRemoved { index: 1, count: 1 }]);
    assert!(!table.moving.rows.has_pending());
}

#[test]
fn a_table_its_view_builds_again_after_a_header_click_moves_its_rows_on() {
    // The frame's instant is held, so the test reads the motion where it
    // starts.
    let _frame = crate::motion::pin_frame();
    let theme = Theme::dark();
    let sort = crate::LocalState::new(None::<TableSort>);
    let build = |id: &str| {
        table(hundred(&["00", "03", "02", "01"]))
            .with_id(id)
            .with_sort_state(&sort)
    };
    let mut clicked = build("rebuilt");
    settled_frames(&mut clicked, &theme);
    crate::motion::note_source(MotionSource::Pointer);
    assert!(clicked.toggle_sort(0).is_some());
    // The view keeps the sort, so it builds the table again.
    drop(clicked);
    let mut table = build("rebuilt");
    frame(&mut table, &theme);
    let names: Vec<_> = table
        .view_rows()
        .iter()
        .take(4)
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(names, ["00", "01", "02", "03"]);
    assert!(
        table.moving.rows.get(&key("03")).is_some() && table.moving.rows.get(&key("01")).is_some(),
        "the rows go on moving in the table that replaced the one clicked"
    );
    assert!(table.moving.rows.get(&key("00")).is_none());
    assert!(Widget::needs_animation_frame(&table));

    // Built again once more, mid-motion: the rows go on, they do not
    // start over.
    let now = crate::motion::frame_now();
    let offset = table
        .moving
        .rows
        .get(&key("03"))
        .expect("moving")
        .offset(now);
    drop(table);
    let mut table = build("rebuilt");
    frame(&mut table, &theme);
    let again = table
        .moving
        .rows
        .get(&key("03"))
        .expect("still moving")
        .offset(now);
    assert!((again - offset).abs() < 0.5);
}

#[test]
fn a_keyed_table_whose_host_reorders_its_rows_moves_them() {
    let _frame = crate::motion::pin_frame();
    let theme = Theme::dark();
    let mut table = table(rows(&["00", "01", "02", "03"]));
    settled_frames(&mut table, &theme);
    let before_02 = table.viewport.item_view_top(2).expect("shown");

    // The person moves 02 to the top; the host hands the rows in again.
    crate::motion::note_source(MotionSource::Pointer);
    table.set_rows(rows(&["02", "00", "01", "03"]));
    frame(&mut table, &theme);
    let now = crate::motion::frame_now();
    let after_02 = table.viewport.item_view_top(0).expect("shown");
    let moving = table.moving.rows.get(&key("02")).expect("02 moves up");
    assert!(
        (moving.offset(now) - (before_02 - after_02)).abs() < 2.0,
        "02 is drawn where it stood"
    );
    let pushed = table.moving.rows.get(&key("00")).expect("00 makes room");
    assert!(pushed.offset(now) < 0.0, "00 is drawn above its new place");
}
