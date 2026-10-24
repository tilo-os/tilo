use super::*;
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct Item {
    name: String,
    depth: usize,
    open: bool,
    children: bool,
    on: bool,
}

fn item(name: &str) -> Item {
    Item {
        name: name.to_owned(),
        depth: 0,
        open: false,
        children: false,
        on: false,
    }
}

fn items(n: usize) -> Vec<Item> {
    (0..n).map(|i| item(&format!("item {i}"))).collect()
}

fn name_col() -> ColumnDef<Item> {
    ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).with_text(|r: &Item| r.name.clone())
}

/// Lay `table` out at `width` × `height` through the measuring pass the
/// host runs, so `Auto` columns and wrapping rows settle.
fn settle(table: &mut DataTable<Item>, width: f32, height: f32) -> (TaffyTree<()>, NodeId) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut node = None;
    let mut tree = TaffyTree::<()>::new();
    for _ in 0..4 {
        tree = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let tnode = table.build_layout(&mut tree, &theme, &mut map);
        let root = tree.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size::from_lengths(width, height),
                ..Default::default()
            },
            &[tnode],
        );
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
        )
        .unwrap();
        let rect = tree.layout(tnode).copied().unwrap_or_default();
        let _ = rect;
        table.apply_layout(&tree, tnode, Point::new(24.0, 0.0));
        let changed = table.adjust_layout_style(&mut tree, tnode, &mut text);
        node = Some(tnode);
        if !changed {
            break;
        }
    }
    (tree, node.unwrap())
}

fn send(
    table: &mut DataTable<Item>,
    tree: &TaffyTree<()>,
    node: NodeId,
    ev: &WindowEvent,
    shift: bool,
) -> bool {
    let mods = if shift {
        winit::keyboard::ModifiersState::SHIFT
    } else {
        winit::keyboard::ModifiersState::empty()
    };
    crate::testing::Dispatch::new()
        .modifiers(mods)
        .layout(tree, node)
        .send(table, ev)
        .handled
}

fn at(x: f64, y: f64) -> WindowEvent {
    crate::synthetic_input::cursor_moved(x, y)
}

fn button(state: ElementState, button: MouseButton) -> WindowEvent {
    crate::synthetic_input::mouse_input(state, button)
}

#[test]
fn the_first_column_starts_on_the_content_edge_and_the_lift_bleeds_past_it() {
    let mut table = DataTable::new(vec![name_col()]).selectable(true);
    table.set_rows(items(3));
    let _ = settle(&mut table, 300.0, 400.0);
    let theme = Theme::dark();
    let bleed = crate::chrome::row_bleed(&theme);
    let rect = table.get_rect();
    let band = table.columns_band();
    assert!(
        (band.x0 - (rect.x0 - bleed)).abs() < 1e-6,
        "{band:?} {rect:?}"
    );
    assert!((band.x1 - (rect.x1 + bleed)).abs() < 1e-6);
    // A cell pads its text by the bleed: the first column's text is on
    // the table's own edge.
    let first = table.col_rects[0];
    assert!((first.x0 + bleed - rect.x0).abs() < 1e-6);
    let row = table.row_rect(0).unwrap();
    assert!((row.x0 - band.x0).abs() < 1e-6 && (row.x1 - band.x1).abs() < 1e-6);
}

#[test]
fn rows_are_the_density_rows_thirty_six_and_twenty_eight_compact() {
    let theme = Theme::dark();
    let mut regular = DataTable::new(vec![name_col()]);
    regular.set_rows(items(2));
    let _ = settle(&mut regular, 300.0, 400.0);
    assert_eq!(regular.viewport.item_height(0), Some(36.0));
    let mut compact = DataTable::new(vec![name_col()]).compact();
    compact.set_rows(items(2));
    let _ = settle(&mut compact, 300.0, 400.0);
    assert_eq!(compact.viewport.item_height(0), Some(28.0));
    let face = DataTableFace::new(&[], &[]);
    assert!((face.row_height(&theme) - 36.0).abs() < 0.01);
    let dense = theme.clone().with_density(crate::tokens::Density::Compact);
    assert!(
        (face.row_height(&dense) - 28.0).abs() < 0.01,
        "a compact region"
    );
    assert!((DataTableFace::header_height(&theme) - regular.header_height()).abs() < 0.01);
    // A two-line row holds a body line over a caption one; a table
    // without its header starts its rows at the top.
    let rows: [&[&str]; 1] = [&[""]];
    let two = DataTableFace::new(&[], &rows).two_line().without_header();
    assert!(two.row_height(&theme) > face.row_height(&theme));
    let rect = Rect::new(0.0, 10.0, 200.0, 200.0);
    assert!((two.row_rect(rect, 0, &theme).y0 - 10.0).abs() < 0.01);
    assert!((two.height(&theme) - two.row_height(&theme)).abs() < 0.01);
}

#[test]
fn narrow_tables_hide_columns_by_priority_then_scroll_sideways() {
    let cols = || {
        vec![
            name_col(),
            ColumnDef::new("a", "A", ColumnWidth::Fixed(120.0)).priority(2),
            ColumnDef::new("b", "B", ColumnWidth::Fixed(120.0)).priority(1),
            ColumnDef::new("c", "C", ColumnWidth::Fixed(120.0)),
        ]
    };
    let mut table = DataTable::new(cols());
    table.set_rows(items(3));
    let _ = settle(&mut table, 600.0, 400.0);
    assert_eq!(table.hidden, [false, false, false, false]);
    let _ = settle(&mut table, 300.0, 400.0);
    assert_eq!(
        table.hidden,
        [false, true, false, false],
        "A gives way first"
    );
    let (tree, node) = settle(&mut table, 120.0, 400.0);
    assert_eq!(table.hidden, [false, true, true, false]);
    assert!(table.h_overflow(), "what still does not fit scrolls");
    let before = table.scroll_x();
    send(&mut table, &tree, node, &at(60.0, 60.0), false);
    let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
    assert!(
        send(&mut table, &tree, node, &wheel, true),
        "shift-wheel scrolls sideways"
    );
    assert!(table.scroll_x() > before);
    assert!(table.h_track().is_some(), "the bar shows");
}

#[test]
fn flexible_minima_keep_identity_readable_before_hiding_or_scrolling() {
    let mut table = DataTable::new(vec![
        name_col().min_width(200.0),
        ColumnDef::new("detail", "Detail", ColumnWidth::Flex(3.0))
            .min_width(64.0)
            .priority(1)
            .with_text(|_: &Item| "detail".to_owned()),
        ColumnDef::new("action", "Action", ColumnWidth::Fixed(80.0)),
    ]);
    table.set_rows(items(3));
    let _ = settle(&mut table, 600.0, 400.0);
    assert_eq!(table.hidden, [false, false, false]);
    assert!(table.col_rects[0].width() >= 200.0);
    assert!(table.col_rects[1].width() >= 64.0);
    assert!(
        table.col_rects[2]
            .x1
            .total_cmp(&table.columns_band().x1)
            .is_eq()
    );
    let _ = settle(&mut table, 320.0, 400.0);
    assert_eq!(table.hidden, [false, true, false]);
    assert!(table.col_rects[0].width() >= 200.0);
    assert!(!table.h_overflow());
    let (tree, node) = settle(&mut table, 240.0, 400.0);
    assert_eq!(table.hidden, [false, true, false]);
    assert!(table.col_rects[0].width().total_cmp(&200.0).is_eq());
    assert!(table.col_rects[2].width().total_cmp(&80.0).is_eq());
    assert!(table.h_overflow());
    let before = table.scroll_x();
    send(&mut table, &tree, node, &at(100.0, 60.0), false);
    let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
    assert!(send(&mut table, &tree, node, &wheel, true));
    assert!(table.scroll_x() > before);
    assert!(table.h_track().is_some());
}

#[test]
fn a_capped_flexible_column_leaves_its_room_to_the_others() {
    let mut table = DataTable::new(vec![
        name_col().max_width(120.0),
        ColumnDef::new("detail", "Detail", ColumnWidth::Flex(2.0))
            .with_text(|_: &Item| "detail".to_owned()),
    ]);
    table.set_rows(items(3));
    let _ = settle(&mut table, 900.0, 400.0);
    assert!(table.col_rects[0].width().total_cmp(&120.0).is_eq());
    assert!(
        table.col_rects[1]
            .x1
            .total_cmp(&table.columns_band().x1)
            .is_eq(),
        "the uncapped column reaches the table's edge"
    );
    let _ = settle(&mut table, 240.0, 400.0);
    assert!(
        table.col_rects[0].width() < 120.0,
        "under the cap it shares"
    );
}

#[test]
fn auto_columns_share_the_leftover_without_a_flexible_one() {
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Auto).with_text(|r: &Item| r.name.clone()),
        ColumnDef::new("kind", "Kind", ColumnWidth::Auto).with_text(|_| "File".to_owned()),
    ]);
    table.set_rows(items(3));
    let _ = settle(&mut table, 500.0, 400.0);
    let band = table.columns_band();
    let last = *table.col_rects.last().unwrap();
    assert!((last.x1 - band.x1).abs() < 0.5, "the columns fill the band");
    assert!(table.col_rects[0].width() > 60.0);
}

#[test]
fn a_tree_table_folds_by_chevron_and_arrows_and_reads_levels() {
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&toggles);
    let mut table = DataTable::new(vec![name_col()]).selectable(true).tree(
        |r: &Item| r.depth,
        |r| r.children,
        |r| r.open,
        move |row, open| seen.borrow_mut().push((row, open)),
    );
    let mut rows = items(3);
    rows[0].children = true;
    rows[1].depth = 1;
    table.set_rows(rows);
    let (tree, node) = settle(&mut table, 300.0, 400.0);
    let chevron = table.tree_chevron(0, &Theme::dark()).expect("a parent row");
    send(
        &mut table,
        &tree,
        node,
        &at(chevron.center().x, chevron.center().y),
        false,
    );
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    assert_eq!(*toggles.borrow(), [(0, true)]);
    let node_a11y = Widget::accessibility(&table).unwrap();
    assert_eq!(node_a11y.children[0].level, Some(1));
    assert_eq!(node_a11y.children[0].state.expanded, Some(false));
    assert_eq!(node_a11y.children[1].level, Some(2));
    let mut doc = AutoCommit::new();
    assert!(Widget::perform_action(
        &mut table,
        &crate::a11y::Action::Expand,
        &[0],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(toggles.borrow().len(), 2);
}

#[test]
fn a_sort_indicator_shows_without_reordering() {
    let mut table = DataTable::new(vec![name_col().sortable(true)]);
    table.set_rows(vec![item("b"), item("a")]);
    table.set_sort_indicator("name", true);
    let names: Vec<String> = table.view_rows().iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["b", "a"]);
    assert_eq!(
        table.sort_indicator.as_ref().map(|(k, a)| (k.as_str(), *a)),
        Some(("name", true))
    );
}

#[test]
fn hosted_checkbox_cells_toggle_instead_of_activating_their_row() {
    struct RetainedTable(DataTable<Item>);
    impl crate::widget::Decorator for RetainedTable {
        type Inner = DataTable<Item>;
        fn inner(&self) -> &Self::Inner {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.0
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }
    let checks = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&checks);
    let activations = Rc::new(Cell::new(0));
    let activated = Rc::clone(&activations);
    let mut table = DataTable::new(vec![
        ColumnDef::checkbox("on", "On", |r: &Item| r.on),
        name_col(),
    ])
    .with_row_key(|row: &Item| u64::from(*row.name.as_bytes().last().unwrap()))
    .selectable(true)
    .on_check(move |row, on| seen.borrow_mut().push((row, on)))
    .on_activate(move |_| activated.set(activated.get() + 1));
    table.set_rows(items(2));
    let mut harness = crate::headless::FrameHarness::new(
        crate::State::new(),
        Box::new(RetainedTable(table)),
        (400, 300),
    );
    let cells: Vec<_> = harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == crate::accesskit::Role::CheckBox)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(cells.len(), 2);
    harness
        .perform_a11y_action(cells[1], &crate::a11y::Action::Focus)
        .unwrap();
    assert!(
        checks.borrow().is_empty(),
        "focus does not toggle a checkbox"
    );
    harness
        .perform_a11y_action(cells[1], &crate::a11y::Action::ToggleCheck)
        .unwrap();
    harness
        .perform_a11y_action(cells[0], &crate::a11y::Action::Activate)
        .unwrap();
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
    assert_eq!(activations.get(), 0, "a checkbox never activates its row");
    harness
        .with_widget_mut::<RetainedTable, _>(|table| table.0.set_rows(vec![item("item 1")]))
        .unwrap();
    harness.relayout();
    assert!(
        harness
            .perform_a11y_action(cells[0], &crate::a11y::Action::ToggleCheck)
            .is_err(),
        "a removed record's retained checkbox must be refused"
    );
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
}

#[test]
fn badges_and_checkboxes_are_measured_painted_and_read() {
    let checks = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&checks);
    let mut table = DataTable::new(vec![
        ColumnDef::checkbox("on", "On", |r: &Item| r.on),
        ColumnDef::new("level", "Level", ColumnWidth::Auto)
            .with_badge(|_| ("Warning".to_owned(), crate::widget::Tone::Warning)),
        name_col(),
    ])
    .selectable(true)
    .on_check(move |row, on| seen.borrow_mut().push((row, on)));
    table.set_rows(items(2));
    let (tree, node) = settle(&mut table, 400.0, 300.0);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let badge = crate::widget::BadgeFace::intrinsic_width("Warning", &mut text, &theme);
    assert!(table.col_rects[1].width() >= badge);
    let row = table.row_rect(1).unwrap();
    let check = table.col_rects[0];
    send(
        &mut table,
        &tree,
        node,
        &at(check.center().x, row.center().y),
        false,
    );
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    assert_eq!(*checks.borrow(), [(1, true)]);
    let mut state = crate::state::State::new();
    assert!(table.perform_action(
        &crate::a11y::Action::ToggleCheck,
        &[0, 0],
        &mut crate::widget::Edits::lend(&mut state.doc),
        &state.root,
    ));
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
    assert!(!table.perform_action(
        &crate::a11y::Action::ToggleCheck,
        &[0, 99],
        &mut crate::widget::Edits::lend(&mut state.doc),
        &state.root,
    ));
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
    let a11y = Widget::accessibility(&table).unwrap();
    let cell = &a11y.children[0].children[0];
    assert_eq!(cell.role, crate::a11y::Role::Checkbox);
    assert_eq!(cell.state.checked, Some(false));
    assert_eq!(
        a11y.children[0].children[1].value.as_deref(),
        Some("Warning")
    );
}

#[test]
fn a_context_menu_selects_its_row_and_a_drag_begins_only_past_the_slop() {
    let menus = Rc::new(RefCell::new(Vec::new()));
    let drags = Rc::new(Cell::new(0));
    let (m, d) = (Rc::clone(&menus), Rc::clone(&drags));
    let mut table = DataTable::new(vec![name_col()])
        .selectable(true)
        .with_viewport(200.0, 40.0)
        .on_row_context_menu(move |row, _, rect| m.borrow_mut().push((row, rect)))
        .on_row_drag_begin(move |_, _, _| d.set(d.get() + 1));
    table.set_rows(items(50));
    let (tree, node) = settle(&mut table, 300.0, 400.0);
    let row = table.row_rect(2).unwrap();
    send(&mut table, &tree, node, &at(60.0, row.center().y), false);
    assert!(send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Right),
        false
    ));
    assert_eq!(menus.borrow().len(), 1);
    assert_eq!(menus.borrow()[0].0, 2);
    assert!(table.is_row_selected(2));
    // A mouse press on a scrolling body selects at once (no drag-scroll
    // unless asked for), and a drag past the slop begins a row drag.
    let row = table.row_rect(1).unwrap();
    send(&mut table, &tree, node, &at(60.0, row.center().y), false);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    assert!(table.is_row_selected(1));
    send(
        &mut table,
        &tree,
        node,
        &at(61.0, row.center().y + 1.0),
        false,
    );
    assert_eq!(drags.get(), 0, "within the slop");
    send(
        &mut table,
        &tree,
        node,
        &at(60.0, row.center().y + 20.0),
        false,
    );
    assert_eq!(drags.get(), 1);
    assert_eq!(
        table.row_at_point(Point::new(60.0, row.center().y)),
        Some(1)
    );
}

#[test]
fn a_plain_press_on_a_selection_drags_it_whole_and_a_still_press_clicks() {
    let drags = Rc::new(Cell::new(0));
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let (d, c) = (Rc::clone(&drags), Rc::clone(&clicks));
    let mut table = DataTable::new(vec![name_col()])
        .selectable(true)
        .with_viewport(200.0, 40.0)
        .on_row_click(move |row, _, _| c.borrow_mut().push(row))
        .on_row_drag_begin(move |_, _, _| d.set(d.get() + 1));
    table.set_rows(items(50));
    let (tree, node) = settle(&mut table, 300.0, 400.0);
    let row = |table: &DataTable<Item>, i: usize| table.row_rect(i).unwrap().center().y;
    // Rows 1 to 3 selected with Shift.
    let y1 = row(&table, 1);
    send(&mut table, &tree, node, &at(60.0, y1), false);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Released, MouseButton::Left),
        false,
    );
    let y3 = row(&table, 3);
    send(&mut table, &tree, node, &at(60.0, y3), true);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        true,
    );
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Released, MouseButton::Left),
        true,
    );
    assert_eq!(table.selected_rows(), [1, 2, 3]);
    clicks.borrow_mut().clear();
    // A plain press on row 2 that drags: the three go.
    let y = row(&table, 2);
    send(&mut table, &tree, node, &at(60.0, y), false);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    assert_eq!(
        table.selected_rows(),
        [1, 2, 3],
        "the press keeps the selection"
    );
    send(&mut table, &tree, node, &at(60.0, y + 20.0), false);
    assert_eq!(drags.get(), 1);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Released, MouseButton::Left),
        false,
    );
    assert_eq!(table.selected_rows(), [1, 2, 3]);
    assert!(clicks.borrow().is_empty(), "a drag is no click");
    // A plain press that does not move is a click on its row.
    send(&mut table, &tree, node, &at(60.0, y), false);
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Pressed, MouseButton::Left),
        false,
    );
    send(
        &mut table,
        &tree,
        node,
        &button(ElementState::Released, MouseButton::Left),
        false,
    );
    assert_eq!(table.selected_rows(), [2]);
    assert_eq!(*clicks.borrow(), [2]);
}

#[test]
fn apply_changes_keeps_cell_controls_mounted_by_key() {
    let builds = Rc::new(Cell::new(0));
    let counter = Rc::clone(&builds);
    let mut table = DataTable::new(vec![
        name_col(),
        ColumnDef::new("edit", "Edit", ColumnWidth::Fixed(80.0)).with_widget(move |r: &Item, _| {
            counter.set(counter.get() + 1);
            Box::new(crate::widget::Text::new(&r.name)) as Element
        }),
    ])
    .with_row_key(|r: &Item| {
        r.name
            .bytes()
            .fold(7_u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)))
    });
    table.set_rows(items(3));
    let _ = settle(&mut table, 400.0, 300.0);
    let first = builds.get();
    assert_eq!(first, 3);
    table.rows_mut().push(Box::new(item("new")));
    table.apply_changes(&[TableModelChange::RowsInserted { index: 3, count: 1 }]);
    let _ = settle(&mut table, 400.0, 300.0);
    assert_eq!(builds.get(), first + 1, "only the new row builds");
}

#[test]
fn a_filling_table_scrolls_inside_its_parent_and_wrapping_rows_grow() {
    let mut table = DataTable::new(vec![
        ColumnDef::new("text", "Message", ColumnWidth::Flex(1.0))
            .with_text(|r: &Item| r.name.clone())
            .wrapping(3),
    ])
    .fill_height();
    let mut rows = items(40);
    rows[0].name = "A message long enough to wrap onto more than one line in a narrow table".into();
    table.set_rows(rows);
    let _ = settle(&mut table, 200.0, 300.0);
    assert!((table.viewport.viewport() - (300.0 - table.header_height())).abs() < 0.5);
    assert!(table.internal_scroll());
    let tall = table.viewport.item_height(0).unwrap();
    let plain = table.viewport.item_height(1).unwrap();
    assert!(tall > plain, "{tall} vs {plain}");
}

#[test]
fn painters_receive_the_table_transform() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let sink = Arc::clone(&seen);
    let mut table = DataTable::new(vec![
        ColumnDef::new("p", "P", ColumnWidth::Flex(1.0)).with_render(Box::new(
            move |_, _, transform, _, _, _| {
                *sink.lock().unwrap() = Some(transform);
            },
        )),
    ]);
    table.set_rows(items(1));
    let _ = settle(&mut table, 200.0, 200.0);
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    let shift = Affine::translate((0.0, -30.0));
    table.draw(
        &mut scene,
        shift,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default(),
    );
    assert_eq!(*seen.lock().unwrap(), Some(shift));
}

#[test]
fn a_table_face_lays_rows_out_and_hit_tests_them() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let columns = [
        FaceColumn::new("Title", ColumnWidth::Flex(1.0)),
        FaceColumn::new("Time", ColumnWidth::Auto).with_kind(TextKind::Numeric),
    ];
    let rows: [&[&str]; 2] = [&["Blue in green", "5:37"], &["So what", "9:22"]];
    let face = DataTableFace::new(&columns, &rows)
        .selected(Some(1))
        .sorted_by(0, true);
    let rect = Rect::new(0.0, 0.0, 400.0, face.height(&theme));
    let cols = face.column_rects(rect, &mut text, &theme);
    let bleed = crate::chrome::row_bleed(&theme);
    assert!((cols[0].x0 + bleed).abs() < 1e-6);
    assert!((cols[1].x1 - (400.0 + bleed)).abs() < 0.5);
    let second = face.row_rect(rect, 1, &theme);
    assert_eq!(face.row_at(rect, second.center(), &theme), Some(1));
    let mut scene = Scene::new();
    face.paint(
        &mut crate::paint::PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
    );
    assert!(scene.encoding().resources.glyph_runs.len() >= 6);
}
