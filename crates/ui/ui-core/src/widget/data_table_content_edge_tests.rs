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

/// Lay `table` out at `width` × `height` through the solve a window makes, which sizes `Auto`
/// columns and wrapping rows.
fn settle(table: &mut DataTable<Item>, width: f32, height: f32) -> (TaffyTree<()>, NodeId) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut tree = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = table.build_layout(&mut tree, &theme, &mut map);
    let root = tree.parent_node(
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_lengths(width, height),
            ..Default::default()
        },
        &[node],
    );
    tilo_ui_base::widget::layout::solve_built(
        table,
        &mut tree,
        (root, node),
        &map,
        Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::Definite(height),
        },
        &mut text,
    )
    .unwrap();
    table.apply_layout(&tree, node, Point::new(24.0, 0.0));
    (tree, node)
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
    tilo_ui_base::synthetic_input::cursor_moved(x, y)
}

fn button(state: ElementState, button: MouseButton) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_input(state, button)
}

tilo_test_env::scenario! {
    /// A table's content edge, row density and how its columns share and give up width.
    /// Each phase was a test of its own and builds its own fixture.
    fn table_columns_keep_the_content_edge_density_and_priorities;


    phase narrow_tables_hide_columns_by_priority_then_scroll_sideways;
    {
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
        let wheel = tilo_ui_base::synthetic_input::mouse_wheel(0.0, -1.0);
        assert!(
            send(&mut table, &tree, node, &wheel, true),
            "shift-wheel scrolls sideways"
        );
        assert!(table.scroll_x() > before);
        assert!(table.h_track().is_some(), "the bar shows");
    }



}

tilo_test_env::scenario! {
    /// Tree rows, the sort indicator, dragging a selection, kept cell controls and the rows of a table face.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_table_folds_sorts_drags_and_lays_out_its_rows;

    phase a_tree_table_folds_by_chevron_and_arrows_and_reads_levels;
    {
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
            &tilo_ui_base::a11y::Action::Expand,
            &[0],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(toggles.borrow().len(), 2);
    }

    phase a_sort_indicator_shows_without_reordering;
    {
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

    phase a_plain_press_on_a_selection_drags_it_whole_and_a_still_press_clicks;
    {
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


}

#[test]
fn hosted_checkbox_cells_toggle_instead_of_activating_their_row() {
    struct RetainedTable(DataTable<Item>);
    impl tilo_ui_base::widget::Decorator for RetainedTable {
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
    let mut harness = crate::app::driver::Driver::new(
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
        .perform_a11y_action(cells[1], &tilo_ui_base::a11y::Action::Focus)
        .unwrap();
    assert!(
        checks.borrow().is_empty(),
        "focus does not toggle a checkbox"
    );
    harness
        .perform_a11y_action(cells[1], &tilo_ui_base::a11y::Action::ToggleCheck)
        .unwrap();
    harness
        .perform_a11y_action(cells[0], &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
    assert_eq!(activations.get(), 0, "a checkbox never activates its row");
    harness
        .with_widget_mut::<RetainedTable, _>(|table| table.0.set_rows(vec![item("item 1")]))
        .unwrap();
    harness.relayout();
    assert!(
        harness
            .perform_a11y_action(cells[0], &tilo_ui_base::a11y::Action::ToggleCheck)
            .is_err(),
        "a removed record's retained checkbox must be refused"
    );
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
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
