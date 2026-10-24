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
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);
    let before_03 = table.viewport.item_view_top(1).expect("shown");
    let before_01 = table.viewport.item_view_top(3).expect("shown");

    crate::motion::note_source(MotionSource::Pointer);
    assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
    frame(&mut table, &theme);
    let now = crate::time::now();

    let names: Vec<_> = table
        .view_rows()
        .iter()
        .take(4)
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(names, ["00", "01", "02", "03"], "the order lands at once");
    let after_03 = table.viewport.item_view_top(3).expect("shown");
    let after_01 = table.viewport.item_view_top(1).expect("shown");
    let moving_03 = table.row_motion.get(&key("03")).expect("03 moves down");
    let moving_01 = table.row_motion.get(&key("01")).expect("01 moves up");
    assert!(
        (moving_03.offset(now) - (before_03 - after_03)).abs() < 2.0,
        "03 is drawn where it stood, above its new place"
    );
    assert!(
        (moving_01.offset(now) - (before_01 - after_01)).abs() < 2.0,
        "01 is drawn where it stood, below its new place"
    );
    assert!(
        table.row_motion.get(&key("00")).is_none() && table.row_motion.get(&key("02")).is_none(),
        "rows the sort left in place do not move"
    );
    assert!(
        table.row_motion.get(&key("50")).is_none(),
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
    assert!(!table.row_motion.is_moving(later));
    table
        .row_motion
        .start(std::iter::empty(), theme.motion(), later);
    assert!(
        table.row_motion.is_empty(),
        "rows that arrived are forgotten"
    );
    let until = crate::motion::frames_until().expect("the sort asked for frames");
    assert!(!crate::motion::frames_wanted(until));
}

#[test]
fn rows_a_click_sort_brings_from_off_screen_fade_in_at_their_slot() {
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "90", "91", "92", "93", "94"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Pointer);
    assert!(table.toggle_sort(0).is_some());
    frame(&mut table, &theme);
    let now = crate::time::now();

    let arriving = table
        .row_motion
        .get(&key("01"))
        .expect("01 came from below");
    assert!(arriving.opacity(now) < 0.5, "it fades in");
    assert!(
        arriving.offset(now).abs() < f64::EPSILON,
        "at its own slot, without travelling across the table"
    );
    assert!(
        table.row_motion.get(&key("90")).is_none(),
        "a row that left the viewport is not drawn on its way out"
    );
    assert!(table.row_motion.get(&key("00")).is_none());
}

#[test]
fn a_sort_by_key_lands_on_the_next_frame() {
    let theme = Theme::dark();
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Keyboard);
    assert!(table.toggle_sort(0).is_some());
    assert!(!table.row_motion.has_pending(), "a key's result lands");
    frame(&mut table, &theme);
    assert!(table.row_motion.is_empty());
    assert!(!table.row_motion.is_moving(crate::time::now()));
}

#[test]
fn under_reduced_motion_a_clicked_sort_lands() {
    let theme = Theme::dark().with_reduce_motion(true);
    let mut table = table(hundred(&["00", "03", "02", "01"]));
    settled_frames(&mut table, &theme);

    crate::motion::note_source(MotionSource::Pointer);
    assert!(table.toggle_sort(0).is_some());
    frame(&mut table, &theme);
    assert!(table.row_motion.is_empty(), "rows land in place at once");
    assert!(!Widget::needs_animation_frame(&table));
}

#[test]
fn a_row_the_person_adds_fades_in_and_pushes_the_shown_rows_down() {
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
    let now = crate::time::now();

    let added = table.row_motion.get(&key("50")).expect("the new row");
    assert!(added.opacity(now) < 0.5, "the added row fades in");
    let after_01 = table.viewport.item_view_top(2).expect("shown");
    let pushed = table.row_motion.get(&key("01")).expect("01 moves down");
    assert!(
        (pushed.offset(now) - (before_01 - after_01)).abs() < 2.0,
        "the row below starts where it stood"
    );
    assert!(table.row_motion.get(&key("00")).is_none());

    // The same change by a key lands.
    crate::motion::note_source(MotionSource::Keyboard);
    table.rows_mut().remove(1);
    table.apply_changes(&[TableModelChange::RowsRemoved { index: 1, count: 1 }]);
    assert!(!table.row_motion.has_pending());
}
