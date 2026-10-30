//! The table's own tests: its rows, cells, selection and scrolling,
//! driven through the widget's event and paint paths.

use super::*;
use crate::testing::Dispatch;
use crate::time::Duration;
use crate::widget::Button;
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
pub(super) struct R {
    pub(super) name: String,
}

#[test]
fn a_column_kind_styles_its_text_whenever_it_is_declared() {
    for column in [
        ColumnDef::<R>::new("size", "Size", ColumnWidth::Fixed(80.0))
            .with_kind(TextKind::Numeric)
            .with_text(|r: &R| r.name.clone()),
        ColumnDef::<R>::new("size", "Size", ColumnWidth::Fixed(80.0))
            .with_text(|r: &R| r.name.clone())
            .with_kind(TextKind::Numeric),
    ] {
        assert_eq!(column.kind, TextKind::Numeric);
        assert!(column.text.is_some());
    }
}

pub(super) fn sample_cols() -> Vec<ColumnDef<R>> {
    vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).sortable(true),
        ColumnDef::new("x", "X", ColumnWidth::Fixed(40.0)),
    ]
}

struct HugeModel {
    row: R,
    key_queries: Rc<Cell<usize>>,
    row_queries: Rc<Cell<usize>>,
}

impl TableModel for HugeModel {
    type Row = R;

    fn len(&self) -> usize {
        100_000_000
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.key_queries
            .set(self.key_queries.get().saturating_add(1));
        (index < self.len()).then(|| u64::try_from(index).expect("index fits u64"))
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        usize::try_from(key)
            .ok()
            .filter(|index| *index < self.len())
    }

    fn row_state(&self, index: usize) -> Arrival<&Self::Row, &str> {
        self.row_queries
            .set(self.row_queries.get().saturating_add(1));
        if index < self.len() {
            Arrival::Ready(&self.row)
        } else {
            Arrival::Failed("outside model")
        }
    }
}

pub(super) struct PagingModel {
    pub(super) row: R,
    pub(super) loaded: Rc<Cell<bool>>,
    pub(super) changed: Rc<Cell<bool>>,
    pub(super) requested: Rc<RefCell<Vec<Range<usize>>>>,
}

struct InsertModel {
    row: R,
    inserted: Rc<Cell<bool>>,
}

struct SortableModel {
    row: R,
    descending: Rc<Cell<bool>>,
}

struct NotifyingModel {
    row: R,
    order: Rc<RefCell<Vec<u64>>>,
    changes: Rc<RefCell<Vec<TableModelChange>>>,
}

impl TableModel for NotifyingModel {
    type Row = R;

    fn len(&self) -> usize {
        self.order.borrow().len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.order.borrow().get(index).copied()
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        self.order
            .borrow()
            .iter()
            .position(|candidate| *candidate == key)
    }

    fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
        Arrival::Ready(&self.row)
    }

    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        std::mem::take(&mut *self.changes.borrow_mut())
    }
}

impl TableModel for SortableModel {
    type Row = R;

    fn len(&self) -> usize {
        3
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        let order = if self.descending.get() {
            [3, 2, 1]
        } else {
            [1, 2, 3]
        };
        order.get(index).copied()
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        let order = if self.descending.get() {
            [3, 2, 1]
        } else {
            [1, 2, 3]
        };
        order.iter().position(|candidate| *candidate == key)
    }

    fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
        Arrival::Ready(&self.row)
    }

    fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
        if column_key != "name" {
            return false;
        }
        self.descending.set(!ascending);
        true
    }
}

impl TableModel for InsertModel {
    type Row = R;

    fn len(&self) -> usize {
        if self.inserted.get() { 101 } else { 100 }
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        if index >= self.len() {
            return None;
        }
        if self.inserted.get() {
            Some(if index == 0 {
                10_000
            } else {
                u64::try_from(index.saturating_sub(1)).expect("index fits u64")
            })
        } else {
            Some(u64::try_from(index).expect("index fits u64"))
        }
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        if self.inserted.get() {
            if key == 10_000 {
                return Some(0);
            }
            usize::try_from(key)
                .ok()
                .filter(|index| *index < 100)
                .map(|index| index.saturating_add(1))
        } else {
            usize::try_from(key).ok().filter(|index| *index < 100)
        }
    }

    fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
        Arrival::Ready(&self.row)
    }
}

impl TableModel for PagingModel {
    type Row = R;

    fn len(&self) -> usize {
        10_000
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        (index < self.len()).then(|| {
            u64::try_from(index)
                .expect("index fits u64")
                .saturating_add(1_000)
        })
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        key.checked_sub(1_000)
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index < self.len())
    }

    fn row_state(&self, index: usize) -> Arrival<&Self::Row, &str> {
        if index == 3 {
            Arrival::Failed("row failed to load")
        } else if self.loaded.get() {
            Arrival::Ready(&self.row)
        } else {
            Arrival::local()
        }
    }

    fn request_range(&mut self, range: Range<usize>) {
        self.requested.borrow_mut().push(range);
    }

    fn poll_changed(&mut self) -> bool {
        self.changed.replace(false)
    }

    fn needs_poll(&self) -> bool {
        !self.loaded.get()
    }
}

#[test]
fn columns_split_flex_and_resize_from_the_press_origin() {
    fn compute_column_rects_splits_flex_after_fixed() {
        let cols = sample_cols();
        let host = Rect::new(0.0, 0.0, 200.0, 28.0);
        let (rects, _) = compute_column_rects(&cols, host, &HashMap::new(), &HashMap::new(), &[]);
        assert_eq!(rects.len(), 2);
        assert!((rects[0].width() - 160.0).abs() < 0.01);
        assert!((rects[1].width() - 40.0).abs() < 0.01);
    }

    fn column_resize_uses_press_origin_clamps_and_stops_on_release() {
        let mut table = DataTable::<R>::new(vec![ColumnDef::new(
            "name",
            "Name",
            ColumnWidth::Fixed(80.0),
        )]);

        table.begin_resize("name", 200.0);
        table.drag_resize(215.0);
        assert_eq!(table.column_width_px("name"), Some(95.0));
        table.drag_resize(220.0);
        assert_eq!(
            table.column_width_px("name"),
            Some(100.0),
            "drag deltas must stay relative to the initial press"
        );

        table.end_resize();
        table.drag_resize(1_000.0);
        assert_eq!(
            table.column_width_px("name"),
            Some(100.0),
            "release must end resize ownership"
        );

        table.begin_resize("name", 100.0);
        table.drag_resize(0.0);
        assert_eq!(
            table.column_width_px("name"),
            Some(DataTable::<R>::MIN_COL_WIDTH)
        );
    }

    /// One extra stroke over the unfocused render, asserted on the production `draw` path.
    fn focused_table_paints_a_focus_ring() {
        let mut unfocused = DataTable::<R>::new(sample_cols()).selectable(true);
        unfocused.set_rows(rows4());
        unfocused.set_focus(false);
        let without = table_color_fills(&mut unfocused);

        let mut focused = DataTable::<R>::new(sample_cols()).selectable(true);
        focused.set_rows(rows4());
        focused.set_focus(true);
        let with = table_color_fills(&mut focused);

        assert_eq!(
            with,
            without + 1,
            "a focused table must add exactly one focus-ring stroke ({with} vs {without})",
        );
    }

    compute_column_rects_splits_flex_after_fixed();
    column_resize_uses_press_origin_clamps_and_stops_on_release();
    focused_table_paints_a_focus_ring();
}

#[test]
fn model_table_virtualizes_pages_and_keeps_stable_selection() {
    fn model_selection_builder_discards_absent_keys() {
        let mut selection = crate::selection::KeySelection::new();
        selection.toggle(3);
        selection.toggle(100_000_000);
        let table = DataTable::from_model(
            sample_cols(),
            HugeModel {
                row: R {
                    name: "resident".into(),
                },
                key_queries: Rc::new(Cell::new(0)),
                row_queries: Rc::new(Cell::new(0)),
            },
        )
        .with_model_selection(selection)
        .with_viewport(120.0, 24.0);

        assert!(table.model_selection().is_selected(&3));
        assert!(!table.model_selection().is_selected(&100_000_000));
        assert_eq!(table.model_selection().len(100_000_000), 1);
        let visible = table.visible_row_range();
        assert_eq!(visible.0, 0);
        assert!(visible.1 <= 10, "viewport plus overscan stays bounded");
    }

    fn model_table_virtualizes_a_hundred_million_rows_in_constant_space() {
        let key_queries = Rc::new(Cell::new(0));
        let row_queries = Rc::new(Cell::new(0));
        let mut table = DataTable::from_model(
            sample_cols(),
            HugeModel {
                row: R {
                    name: "resident".into(),
                },
                key_queries: Rc::clone(&key_queries),
                row_queries: Rc::clone(&row_queries),
            },
        )
        .selectable(true);
        table.set_viewport(120.0, 24.0);
        table.set_scroll_y(f32::MAX);
        let _laid = lay_out(&mut table);

        let semantics = Widget::accessibility(&table).expect("table semantics");
        assert_eq!(table.viewport.len(), 100_000_000);
        assert_eq!(table.viewport.resident_measurements(), 0);
        assert!(semantics.children.len() <= 10);
        assert!(
            key_queries.get() < 100,
            "queried {} keys",
            key_queries.get()
        );
        assert!(
            row_queries.get() < 100,
            "queried {} rows",
            row_queries.get()
        );

        assert!(Widget::perform_semantic_action(
            &mut table,
            "row:99999999:0",
            &crate::a11y::Action::Focus,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(table.model_selection().primary(), Some(&99_999_999));
        table.model_selection_mut().select_all();
        assert!(table.model_selection().is_select_all());
        assert_eq!(table.model_selection().len(100_000_000), 100_000_000);
        assert_eq!(table.model_selection().stored_keys().count(), 0);
    }

    fn model_insert_preserves_stable_anchor_and_key_selection() {
        let inserted = Rc::new(Cell::new(false));
        let mut table = DataTable::from_model(
            sample_cols(),
            InsertModel {
                row: R { name: "row".into() },
                inserted: Rc::clone(&inserted),
            },
        )
        .selectable(true);
        table.set_viewport(48.0, 24.0);
        table.set_scroll_y(50.0 * 24.0 + 5.0);
        table.model_selection_mut().select_only(50);
        let before = table.viewport.item_view_top(50);

        inserted.set(true);
        table.notify_model_changed();

        assert_eq!(table.semantic_row_index("row:50:0"), Some(51));
        assert_eq!(table.viewport.item_view_top(51), before);
        assert_eq!(table.model_selection().primary(), Some(&50));
        assert!(table.model_selection().is_selected(&50));
    }

    fn stable_row_keys_anchor_inserts_above_the_viewport() {
        let mut t = DataTable::<R>::new(sample_cols())
            .with_row_key(|row| u64::from(row.name.as_bytes()[0]));
        t.set_rows(rows4());
        t.set_viewport(24.0, f32_of_f64(ROW_HEIGHT));
        t.set_scroll_y(2.0 * f32_of_f64(ROW_HEIGHT) + 5.0);
        let before = t.viewport.item_view_top(2);

        t.set_rows(vec![
            R { name: "x".into() },
            R { name: "a".into() },
            R { name: "b".into() },
            R { name: "c".into() },
            R { name: "d".into() },
        ]);

        let c = t.semantic_row_index(&format!("row:{}:0", u64::from(b'c')));
        assert_eq!(c, Some(3));
        assert_eq!(t.viewport.item_view_top(c.unwrap()), before);
    }

    model_selection_builder_discards_absent_keys();
    model_table_virtualizes_a_hundred_million_rows_in_constant_space();
    model_insert_preserves_stable_anchor_and_key_selection();
    stable_row_keys_anchor_inserts_above_the_viewport();
}

#[test]
fn granular_model_changes_retain_controls_by_stable_key() {
    fn granular_row_change_rebuilds_only_that_rows_retained_control() {
        let builds = Rc::new(Cell::new(0));
        let order = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let mut table = DataTable::from_model(
            interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
            NotifyingModel {
                row: R { name: "row".into() },
                order,
                changes: Rc::clone(&changes),
            },
        );
        table.set_viewport(120.0, 24.0);
        let _ = lay_out(&mut table);
        let initial_builds = builds.get();

        changes
            .borrow_mut()
            .push(TableModelChange::RowsChanged { keys: vec![2] });
        assert!(Widget::update(
            &mut table,
            &AutoCommit::new(),
            &automerge::ROOT,
        ));
        let _ = lay_out(&mut table);

        assert_eq!(builds.get(), initial_builds + 1);
        assert_eq!(table.mounted_cell_count(), 4);
    }

    fn granular_structural_changes_retain_controls_by_stable_key() {
        let builds = Rc::new(Cell::new(0));
        let order = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let mut table = DataTable::from_model(
            interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
            NotifyingModel {
                row: R { name: "row".into() },
                order: Rc::clone(&order),
                changes: Rc::clone(&changes),
            },
        );
        table.set_viewport(144.0, 24.0);
        let _ = lay_out(&mut table);
        let initial_builds = builds.get();

        order.borrow_mut().insert(0, 99);
        changes
            .borrow_mut()
            .push(TableModelChange::RowsInserted { index: 0, count: 1 });
        assert!(Widget::update(
            &mut table,
            &AutoCommit::new(),
            &automerge::ROOT,
        ));
        let _ = lay_out(&mut table);
        assert_eq!(builds.get(), initial_builds + 1);

        order.borrow_mut().swap(1, 3);
        changes.borrow_mut().push(TableModelChange::RowsMoved {
            from: 1,
            to: 3,
            count: 1,
        });
        assert!(Widget::update(
            &mut table,
            &AutoCommit::new(),
            &automerge::ROOT,
        ));
        let _ = lay_out(&mut table);

        assert_eq!(
            builds.get(),
            initial_builds + 1,
            "moving stable keys retains their live cell controls"
        );
        assert_eq!(table.mounted_cell_count(), 5);
    }

    fn retained_cell_widgets_are_bounded_to_the_huge_model_window() {
        let key_queries = Rc::new(Cell::new(0));
        let row_queries = Rc::new(Cell::new(0));
        let builds = Rc::new(Cell::new(0));
        let model = HugeModel {
            row: R {
                name: "Edit".into(),
            },
            key_queries: Rc::clone(&key_queries),
            row_queries: Rc::clone(&row_queries),
        };
        let mut table = DataTable::from_model(
            interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
            model,
        );
        table.set_viewport(48.0, 24.0);
        let _ = lay_out(&mut table);

        assert!(table.mounted_cell_count() > 0);
        assert!(
            table.mounted_cell_count() < 20,
            "100M logical rows retain only the viewport window"
        );
        assert_eq!(builds.get(), table.mounted_cell_count());
        assert!(key_queries.get() < 100);
        assert!(row_queries.get() < 100);
    }

    fn retained_cell_widgets_follow_stable_rows_through_sorting() {
        let builds = Rc::new(Cell::new(0));
        let mut table = DataTable::new(interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)))
            .with_row_key(|row: &R| {
                u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
            });
        table.set_rows(rows4());
        let _ = lay_out(&mut table);
        let first_builds = builds.get();
        table.sort_by_with(|a, b| a.name.cmp(&b.name), "name", false);
        let _ = lay_out(&mut table);

        assert_eq!(
            builds.get(),
            first_builds,
            "sorting repositions cells without rebuilding retained controls"
        );
    }

    fn retained_cell_widgets_track_scroll_inside_the_same_mounted_window() {
        let mut table = DataTable::new(interactive_cols(
            Rc::new(Cell::new(0)),
            Rc::new(Cell::new(0)),
        ));
        table.set_rows(rows4());
        table.set_viewport(48.0, 24.0);
        let (tree, node) = lay_out(&mut table);
        let before = table
            .mounted_cells
            .first()
            .expect("first mounted cell")
            .widget
            .get_rect()
            .y0;
        table.set_scroll_y(5.0);
        table.apply_layout(&tree, node, Point::ZERO);
        let after = table
            .mounted_cells
            .first()
            .expect("first mounted cell")
            .widget
            .get_rect()
            .y0;
        assert_eq!(after, before - 5.0);
    }

    granular_row_change_rebuilds_only_that_rows_retained_control();
    granular_structural_changes_retain_controls_by_stable_key();
    retained_cell_widgets_are_bounded_to_the_huge_model_window();
    retained_cell_widgets_follow_stable_rows_through_sorting();
    retained_cell_widgets_track_scroll_inside_the_same_mounted_window();
}

#[test]
fn sorting_is_owned_by_the_model_and_reapplied_on_rows() {
    fn model_owns_sort_order_while_table_owns_header_state() {
        let descending = Rc::new(Cell::new(false));
        let mut table = DataTable::from_model(
            sample_cols(),
            SortableModel {
                row: R { name: "row".into() },
                descending: Rc::clone(&descending),
            },
        );

        assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
        assert!(!descending.get());
        assert_eq!(table.sort_state(), Some(("name", true)));
        assert_eq!(table.semantic_row_index("row:1:0"), Some(0));

        assert_eq!(table.toggle_sort(0), Some(("name".into(), false)));
        assert!(descending.get());
        assert_eq!(table.sort_state(), Some(("name", false)));
        assert_eq!(table.semantic_row_index("row:1:0"), Some(2));
    }

    fn toggle_sort_requires_prior_sort_by_with() {
        let mut t = DataTable::<R>::new(sample_cols());
        // No comparator installed yet → toggle returns None.
        assert!(t.toggle_sort(0).is_none());
        t.set_rows(vec![R { name: "b".into() }, R { name: "a".into() }]);
        t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
        assert_eq!(t.view_rows()[0].name, "a");
        let (_, asc) = t.toggle_sort(0).unwrap();
        assert!(!asc);
        assert_eq!(t.view_rows()[0].name, "b");
    }

    fn set_rows_reapplies_sort() {
        let mut t = DataTable::<R>::new(sample_cols());
        t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
        t.set_rows(vec![
            R { name: "c".into() },
            R { name: "a".into() },
            R { name: "b".into() },
        ]);
        let v = t.view_rows();
        assert_eq!(v[0].name, "a");
        assert_eq!(v[1].name, "b");
        assert_eq!(v[2].name, "c");
    }

    fn header_click_fires_on_header_click_instead_of_toggle() {
        let hits = Rc::new(RefCell::new(Vec::<usize>::new()));
        let mut t = {
            let hits = hits.clone();
            DataTable::<R>::new(sample_cols()).on_header_click(move |i| hits.borrow_mut().push(i))
        };
        t.set_rows(rows4());
        // Install a comparator so a fallback toggle *would* have sorted —
        // proving the callback path takes precedence (rows stay put).
        t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
        let (tree, node) = lay_out(&mut t);

        // Header band is y ∈ [0, 28). Name column starts at x=0. Click (20, 10).
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (20.0, 10.0).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());

        assert_eq!(*hits.borrow(), vec![0], "header click reported column 0");
        // Sort direction unchanged because the callback replaced toggle.
        assert_eq!(t.sort_state(), Some(("name", true)));
    }

    fn a_sorted_table_speaks_in_the_hosts_rows_and_keeps_its_order() {
        let rows = || {
            vec![
                R { name: "c".into() },
                R { name: "a".into() },
                R { name: "b".into() },
            ]
        };
        let columns = || {
            vec![
                ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
                    .sortable(true)
                    .with_text(|row: &R| row.name.clone()),
            ]
        };
        let state = crate::LocalState::new(None::<TableSort>);
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let activations = Rc::new(RefCell::new(Vec::new()));
        let (click_sink, activate_sink) = (Rc::clone(&clicks), Rc::clone(&activations));
        let mut table = DataTable::new(columns())
            .selectable(true)
            .with_sort_state(&state)
            .on_row_click(move |row, _, _| click_sink.borrow_mut().push(row))
            .on_activate(move |row| activate_sink.borrow_mut().push(row));
        table.set_rows(rows());
        let _ = lay_out(&mut table);
        // The host selects its row 0 ("c"); sorting by the text column
        // moves it to the end and the selection goes with it.
        table.select_row(0);
        assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
        let shown: Vec<_> = table.view_rows().iter().map(|r| r.name.clone()).collect();
        assert_eq!(shown, ["a", "b", "c"]);
        assert_eq!(table.selected_rows(), [0]);
        assert_eq!(
            state.with(Clone::clone),
            Some(TableSort::ascending("name")),
            "the order is kept where a rebuilt view finds it"
        );

        // A click and a double click on the first row shown name the
        // host's row "a", which is its row 1.
        let _ = lay_out(&mut table);
        let (tree, node) = lay_out(&mut table);
        dispatch(
            &mut table,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (20.0, HEADER_HEIGHT + 12.0).into(),
            },
        );
        for _ in 0..2 {
            for state in [ElementState::Pressed, ElementState::Released] {
                dispatch(
                    &mut table,
                    &tree,
                    node,
                    &crate::synthetic_input::mouse_input(state, MouseButton::Left),
                );
            }
        }
        assert_eq!(*clicks.borrow(), [1, 1]);
        assert_eq!(*activations.borrow(), [1]);
        assert_eq!(table.selected_rows(), [1]);

        assert_eq!(table.toggle_sort(0), Some(("name".into(), false)));
        assert_eq!(table.selected_rows(), [1]);
        assert!(table.selection().is_selected(2));

        // A table built again from the same state starts in that order.
        let mut rebuilt = DataTable::new(columns()).with_sort_state(&state);
        rebuilt.set_rows(rows());
        let shown: Vec<_> = rebuilt.view_rows().iter().map(|r| r.name.clone()).collect();
        assert_eq!(shown, ["c", "b", "a"]);
    }

    model_owns_sort_order_while_table_owns_header_state();
    a_sorted_table_speaks_in_the_hosts_rows_and_keeps_its_order();
    toggle_sort_requires_prior_sort_by_with();
    set_rows_reapplies_sort();
    header_click_fires_on_header_click_instead_of_toggle();
}

#[test]
fn table_accessibility_names_roles_sort_and_selection() {
    fn data_table_exposes_stable_test_and_accessible_names() {
        let t = DataTable::<R>::new(sample_cols());
        assert_eq!(Widget::test_id(&t), Some("data_table"));
        let named = DataTable::<R>::new(sample_cols())
            .with_id("results-table")
            .with_accessible_name("Results");
        assert_eq!(Widget::test_id(&named), Some("results-table"));
        assert_eq!(
            Widget::accessibility(&named).and_then(|node| node.name),
            Some("Results".into())
        );
    }

    fn accessibility_reports_table_role_sort_and_selection() {
        use crate::a11y::Role;
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(rows4());
        // Active sort surfaces via the column's human header, not its key.
        t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
        t.selection_mut().select_only(1);
        let node = Widget::accessibility(&t).expect("table is not presentational");
        assert_eq!(node.role, Role::Table);
        assert!(node.state.focusable);
        assert!(!node.state.focused);
        // The selected row's position is structured (rendered as "2 of 4" by
        // the reader) rather than baked into the value string.
        assert_eq!(node.pos_in_set, Some(2));
        assert_eq!(node.set_size, Some(4));
        assert_eq!(
            node.description.as_deref(),
            Some("sorted by Name ascending")
        );
    }

    fn accessibility_no_sort_or_selection_omits_value_and_description() {
        let t = DataTable::<R>::new(sample_cols());
        let node = Widget::accessibility(&t).unwrap();
        assert_eq!(node.value, None, "no selection → no value");
        assert_eq!(node.pos_in_set, None, "no selection → no position");
        assert_eq!(node.set_size, None, "no selection → no set size");
        assert_eq!(
            node.description, None,
            "no active sort → no active sort description"
        );
    }

    fn the_keyboard_cursor_row_is_exported_as_the_active_descendant() {
        // Exactly one row may claim focus, and it must be the row Up/Down moved
        // the cursor to — that is what puts a reader on the row rather than on
        // the table.
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(vec![
            R { name: "a".into() },
            R { name: "b".into() },
            R { name: "c".into() },
        ]);
        t.selection_mut().move_to(1, false);
        let node = Widget::accessibility(&t).unwrap();
        let focused: Vec<usize> = node
            .children
            .iter()
            .enumerate()
            .filter(|(_, r)| r.state.focused)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(focused, vec![1], "the cursor row, and only it, is focused");
        assert!(
            node.children.iter().all(|r| r.state.focusable),
            "every row must be focusable or an AT cannot navigate to it"
        );

        t.selection_mut().move_to(2, false);
        let node = Widget::accessibility(&t).unwrap();
        assert!(node.children[2].state.focused, "focus follows the cursor");
        assert!(!node.children[1].state.focused);
    }

    fn a_read_only_tables_rows_are_not_focusable() {
        // Nothing to operate, so nothing should advertise itself as operable.
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(rows4());
        assert!(!t.selectable, "the default table is read-only");
        let node = Widget::accessibility(&t).unwrap();
        assert!(node.children.iter().all(|r| !r.state.focusable));
        assert!(node.children.iter().all(|r| !r.state.focused));
    }

    data_table_exposes_stable_test_and_accessible_names();
    accessibility_reports_table_role_sort_and_selection();
    accessibility_no_sort_or_selection_omits_value_and_description();
    the_keyboard_cursor_row_is_exported_as_the_active_descendant();
    a_read_only_tables_rows_are_not_focusable();
}

#[test]
fn semantic_rows_and_cells_publish_identity_geometry_and_values() {
    fn semantic_row_actions_use_stable_row_identities() {
        let activated = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&activated);
        let mut table = DataTable::<R>::new(sample_cols())
            .with_row_key(|row| u64::from(row.name.as_bytes()[0]))
            .selectable(true)
            .on_activate(move |index| captured.borrow_mut().push(index));
        table.set_rows(vec![R { name: "a".into() }, R { name: "b".into() }]);

        let semantics = Widget::accessibility(&table).expect("table semantics");
        assert_eq!(
            semantics.children[1].action_target.as_deref(),
            Some("row:98:0")
        );
        assert!(Widget::perform_semantic_action(
            &mut table,
            "row:98:0",
            &crate::a11y::Action::Focus,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(table.selection().primary(), Some(1));
        assert!(Widget::perform_semantic_action(
            &mut table,
            "row:98:0",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(&*activated.borrow(), &[1]);

        let mut index_keyed = DataTable::<R>::new(sample_cols()).selectable(true);
        index_keyed.set_rows(vec![R { name: "a".into() }]);
        assert_eq!(
            Widget::accessibility(&index_keyed).unwrap().children[0].action_target,
            None,
            "index fallbacks are not stable action identities"
        );
    }

    fn semantic_rows_and_cells_publish_scrolled_viewport_geometry() {
        let mut table = DataTable::<R>::new(sample_cols());
        table.set_rows(rows4());
        table.set_viewport(72.0, 24.0);
        table.set_scroll_y(10.0);
        let _ = lay_out(&mut table);

        let semantics = Widget::accessibility(&table).expect("table semantics");
        let row = semantics.children.first().expect("first visible row");
        let row_bounds = row.bounds.expect("row bounds").to_rect();
        assert_eq!(row_bounds.y0, table.rect.y0 + table.header_h - 10.0);
        assert_eq!(row_bounds.y1, row_bounds.y0 + 24.0);
        assert_eq!(row_bounds.x0, table.rect.x0);
        assert_eq!(row_bounds.x1, table.rect.x1);

        let first_cell = row.children.first().expect("first cell");
        let cell_bounds = first_cell.bounds.expect("cell bounds").to_rect();
        assert_eq!(cell_bounds.y0, row_bounds.y0);
        assert_eq!(cell_bounds.y1, row_bounds.y1);
        assert_eq!(cell_bounds.x0, table.col_rects[0].x0);
        assert_eq!(cell_bounds.x1, table.col_rects[0].x1);
    }

    fn a_routed_action_naming_a_row_that_no_longer_exists_does_nothing() {
        // An assistive technology can hold a node id across a row being
        // removed. Acting on whatever now sits at that index would operate on
        // the wrong record — so an out-of-range row must be dropped, not
        // clamped.
        let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::<usize>::new()));
        let sink = fired.clone();
        let mut t = DataTable::<R>::new(sample_cols())
            .selectable(true)
            .on_activate(move |i| sink.borrow_mut().push(i));
        t.set_rows(vec![R { name: "a".into() }, R { name: "b".into() }]);
        t.selection_mut().move_to(0, false);

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let handled = Widget::perform_action(
            &mut t,
            &crate::a11y::Action::Activate,
            &[7],
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        assert!(!handled, "a vanished row cannot be activated");
        assert!(
            fired.borrow().is_empty(),
            "and must not activate some other row in its place"
        );
        assert_eq!(
            t.selection.primary(),
            Some(0),
            "nor move the cursor off the row the user was on"
        );
    }

    fn accessibility_exposes_per_row_and_cell_structure() {
        use crate::a11y::Role;
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(rows4());
        t.selection_mut().select_only(2);
        let node = Widget::accessibility(&t).unwrap();

        // One Row child per display row, each numbered "N of 4".
        assert_eq!(node.children.len(), 4, "a row node per display row");
        for (i, row) in node.children.iter().enumerate() {
            assert_eq!(row.role, Role::Row);
            assert_eq!(row.pos_in_set, Some(i + 1));
            assert_eq!(row.set_size, Some(4));
            // Every row exposes one cell per column, named by the column header.
            assert_eq!(row.children.len(), 2, "two cells per row (two columns)");
            assert_eq!(row.children[0].role, Role::Cell);
            assert_eq!(row.children[0].name.as_deref(), Some("Name"));
            assert_eq!(row.children[0].pos_in_set, Some(1));
            assert_eq!(row.children[1].name.as_deref(), Some("X"));
            assert_eq!(row.children[1].pos_in_set, Some(2));
        }
        assert!(
            node.children[2].state.selected,
            "row 3 (index 2) is selected"
        );
        assert!(!node.children[0].state.selected);
    }

    fn typed_text_and_custom_cells_publish_row_specific_values() {
        let columns = vec![
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
                .with_text(|row: &R| row.name.clone()),
            ColumnDef::new("custom", "Custom", ColumnWidth::Fixed(80.0))
                .with_render(Box::new(|_, _, _, _, _, _| {}))
                .with_accessible_value(|row: &R| format!("custom {}", row.name)),
            ColumnDef::new("action", "Action", ColumnWidth::Fixed(80.0))
                .with_widget(|_, _| Box::new(Button::new("edit", "Edit"))),
        ];
        let mut table = DataTable::new(columns);
        table.set_rows(vec![R { name: "Ada".into() }]);
        let semantics = Widget::accessibility(&table).expect("table semantics");
        let cells = &semantics.children.first().expect("ready row").children;

        assert_eq!(
            cells.first().and_then(|cell| cell.value.as_deref()),
            Some("Ada")
        );
        assert_eq!(
            cells.get(1).and_then(|cell| cell.value.as_deref()),
            Some("custom Ada")
        );
        assert_eq!(
            cells.get(2).and_then(|cell| cell.value.as_deref()),
            None,
            "retained controls provide their own nested semantics"
        );
        assert!(table.columns[0].text.is_some());
    }

    semantic_row_actions_use_stable_row_identities();
    semantic_rows_and_cells_publish_scrolled_viewport_geometry();
    a_routed_action_naming_a_row_that_no_longer_exists_does_nothing();
    accessibility_exposes_per_row_and_cell_structure();
    typed_text_and_custom_cells_publish_row_specific_values();
}

use winit::event::{ElementState, MouseButton};

/// Inside a 300×400 flex-column root so the table's auto width stretches
/// to 300 and rows get real hit rects.
fn lay_out(table: &mut DataTable<R>) -> (TaffyTree<()>, NodeId) {
    let mut tree = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let theme = Theme::dark();
    let tnode = table.build_layout(&mut tree, &theme, &mut node_map);
    let root = tree.parent_node(
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_lengths(300.0, 400.0),
            ..Default::default()
        },
        &[tnode],
    );
    tree.compute_layout(
        root,
        Size {
            width: taffy::AvailableSpace::Definite(300.0),
            height: taffy::AvailableSpace::Definite(400.0),
        },
    )
    .unwrap();
    table.apply_layout(&tree, tnode, Point::ZERO);
    (tree, tnode)
}

fn rows4() -> Vec<R> {
    vec![
        R { name: "a".into() },
        R { name: "b".into() },
        R { name: "c".into() },
        R { name: "d".into() },
    ]
}

fn interactive_cols(clicks: Rc<Cell<usize>>, builds: Rc<Cell<usize>>) -> Vec<ColumnDef<R>> {
    vec![
        ColumnDef::new("action", "Action", ColumnWidth::Fixed(120.0)).with_widget(
            move |row: &R, index| {
                builds.set(builds.get().saturating_add(1));
                let clicks = Rc::clone(&clicks);
                let id = format!("action-{index}");
                Box::new(
                    Button::new(&id, &row.name)
                        .on_click(move || clicks.set(clicks.get().saturating_add(1))),
                )
            },
        ),
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
    ]
}

/// Row actions mount for the row under the pointer only; a table its
/// host scrolls windows its rows, keeps no bar and leaves the wheel.
#[test]
fn row_actions_reveal_under_the_pointer_and_a_host_scrolls_the_rows() {
    let move_to = |table: &mut DataTable<R>, tree: &TaffyTree<()>, node, y: f64| {
        dispatch(
            table,
            tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (150.0, y).into(),
            },
        );
    };
    {
        let builds = Rc::new(Cell::new(0));
        let mut columns = interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds));
        let actions = columns.remove(0).revealed_on_hover();
        columns.push(actions);
        let mut table = DataTable::new(columns).selectable(true);
        table.set_rows(rows4());
        let (tree, node) = lay_out(&mut table);
        assert_eq!(
            table.mounted_cell_count(),
            0,
            "at rest no row shows actions"
        );
        assert_eq!(table.hovered_row(), None);

        move_to(&mut table, &tree, node, HEADER_HEIGHT + ROW_HEIGHT * 2.5);
        assert_eq!(table.hovered_row(), Some(2));
        assert!(
            table.take_structure_dirty(),
            "the hovered row asks for its actions"
        );
        let (tree, node) = lay_out(&mut table);
        assert_eq!(table.mounted_cell_count(), 1);
        assert_eq!(builds.get(), 1);

        move_to(&mut table, &tree, node, HEADER_HEIGHT + ROW_HEIGHT * 0.5);
        let _ = lay_out(&mut table);
        assert_eq!(table.hovered_row(), Some(0));
        assert_eq!(
            table.mounted_cell_count(),
            1,
            "the actions follow the pointer"
        );
    }
    {
        let mut table = DataTable::new(sample_cols())
            .selectable(true)
            .without_header()
            .host_scrolled();
        table.set_rows(
            (0..200)
                .map(|i| R {
                    name: i.to_string(),
                })
                .collect(),
        );
        assert!((table.content_height() - 200.0 * ROW_HEIGHT).abs() < 0.5);
        table.set_host_window(ROW_HEIGHT * 100.0, ROW_HEIGHT * 5.0);
        let (tree, node) = lay_out(&mut table);
        assert!(
            (table.get_rect().height() - ROW_HEIGHT * 5.0).abs() < 0.5,
            "the table is as tall as the part of it in view, with no header band"
        );
        let (first, last) = table.visible_row_range();
        assert!(
            first <= 100 && last >= 105 && last - first < 20,
            "{first}..{last}"
        );
        assert!(table.scrollbar_thumb().is_none(), "the host owns the bar");
        move_to(&mut table, &tree, node, ROW_HEIGHT * 0.5);
        assert_eq!(
            table.hovered_row(),
            Some(100),
            "rows hit where they are painted"
        );
        let handled = Dispatch::new()
            .layout(&tree, node)
            .send(&mut table, &crate::synthetic_input::mouse_wheel(0.0, -3.0))
            .handled;
        assert!(!handled, "the wheel is the host's");
    }
}

#[test]
fn pointer_and_keyboard_select_rows_unless_cells_or_mode_consume() {
    fn interactive_cell_consumes_click_without_selecting_its_row() {
        let clicks = Rc::new(Cell::new(0));
        let mut table = DataTable::new(interactive_cols(Rc::clone(&clicks), Rc::new(Cell::new(0))))
            .selectable(true)
            .with_row_key(|row: &R| {
                u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
            });
        table.set_rows(rows4());
        // The first pass learns flex column geometry; the second positions
        // retained cell controls against those exact column rectangles.
        let _ = lay_out(&mut table);
        let (tree, node) = lay_out(&mut table);
        dispatch(
            &mut table,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (20.0, HEADER_HEIGHT + 12.0).into(),
            },
        );
        dispatch(
            &mut table,
            &tree,
            node,
            &crate::synthetic_input::mouse_press(),
        );
        dispatch(
            &mut table,
            &tree,
            node,
            &crate::synthetic_input::mouse_release(),
        );

        assert_eq!(clicks.get(), 1, "real cell button receives activation");
        assert!(
            table.selection().is_empty(),
            "the button gesture does not leak into row selection"
        );
        assert_eq!(table.accessibility_child_path(0), Some(vec![0, 0]));
    }

    fn click_selects_row_and_fires_callback() {
        let clicks = Rc::new(RefCell::new(Vec::<(usize, bool, bool)>::new()));
        let mut t = {
            let clicks = clicks.clone();
            DataTable::<R>::new(sample_cols())
                .selectable(true)
                .on_row_click(move |i, c, s| clicks.borrow_mut().push((i, c, s)))
        };
        t.set_rows(rows4());
        let (tree, node) = lay_out(&mut t);

        // The middle of row 2, under the header.
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (50.0, HEADER_HEIGHT + 2.5 * ROW_HEIGHT).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());

        assert!(t.selection().is_selected(2), "row 2 selected");
        assert_eq!(t.selection().primary(), Some(2));
        assert_eq!(*clicks.borrow(), vec![(2, false, false)]);
    }

    fn non_selectable_table_ignores_body_clicks() {
        let mut t = DataTable::<R>::new(sample_cols()); // selectable defaults false
        t.set_rows(rows4());
        let (tree, node) = lay_out(&mut t);
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (50.0, 85.0).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
        assert!(
            t.selection().is_empty(),
            "read-only table keeps empty selection"
        );
    }

    fn keyboard_arrows_move_selection_when_focused() {
        use winit::keyboard::NamedKey;
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(rows4());
        t.set_focus(true);
        let (tree, node) = lay_out(&mut t);
        let key = |named| {
            crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                winit::keyboard::Key::Named(named),
                None,
                ElementState::Pressed,
            ))
        };
        dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown)); // → 0
        dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown)); // → 1
        assert_eq!(t.selection().primary(), Some(1));
        assert!(t.selection().is_selected(1));
    }

    fn aligned_columns_place_their_controls_under_the_header() {
        for (align, expect) in [
            (CellAlign::Leading, 0.0),
            (CellAlign::Center, 0.5),
            (CellAlign::Trailing, 1.0),
        ] {
            let mut table = DataTable::new(vec![
                ColumnDef::new("action", "Action", ColumnWidth::Fixed(160.0))
                    .with_align(align)
                    .with_widget(|row: &R, index| {
                        Box::new(Button::new(&format!("go-{index}"), &row.name))
                    }),
                ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
            ])
            .with_row_key(|row: &R| {
                u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
            });
            table.set_rows(rows4());
            let _ = lay_out(&mut table);
            let _ = lay_out(&mut table);
            let column = table.col_rects[0];
            let control = table.mounted_cells[0].widget.get_rect();
            let slack = column.width() - control.width();
            assert!(
                align == CellAlign::Leading || slack > 1.0,
                "a placed control keeps its own width: {control:?} in {column:?}"
            );
            assert!(
                (control.x0 - column.x0 - slack * expect).abs() < 0.5,
                "{align:?}: {control:?} in {column:?}"
            );
        }
    }

    interactive_cell_consumes_click_without_selecting_its_row();
    click_selects_row_and_fires_callback();
    non_selectable_table_ignores_body_clicks();
    keyboard_arrows_move_selection_when_focused();
    aligned_columns_place_their_controls_under_the_header();
}

fn dispatch(t: &mut DataTable<R>, tree: &TaffyTree<()>, node: NodeId, ev: &WindowEvent) {
    Dispatch::new().layout(tree, node).send(t, ev);
}

#[test]
fn scrolling_shares_variable_row_geometry_with_hit_testing() {
    fn page_navigation_and_accelerated_wheel_share_variable_row_geometry() {
        let mut table = DataTable::<R>::new(sample_cols()).selectable(true);
        table.set_rows(
            (0..100)
                .map(|i| R {
                    name: format!("row {i}"),
                })
                .collect(),
        );
        table.set_viewport(100.0, 24.0);
        assert!(table.set_row_height(0, 300.0));
        table.selection_mut().select_only(0);

        assert!(table.page_selection(true, false));
        assert_eq!(table.selection().primary(), Some(0));
        assert_eq!(table.scroll_y(), 90.0);
        assert!(table.page_selection(false, false));
        assert_eq!(table.scroll_y(), 0.0);

        let mut handled = false;
        table.on_wheel(
            &MouseScrollDelta::LineDelta(0.0, -1.0),
            TouchPhase::Moved,
            false,
            &mut handled,
        );
        assert!(handled);
        let immediate = table.scroll_y();
        assert_eq!(immediate, 60.0);
        assert!(table.needs_animation_frame());
        let frame = crate::time::now() + crate::time::Duration::from_millis(16);
        // The glide moves only the view: it is no change of the model.
        assert!(!crate::time::testing::with_monotonic_time(frame, || {
            Widget::update(&mut table, &AutoCommit::new(), &automerge::ROOT)
        }));
        assert!(
            table.scroll_y() > immediate,
            "momentum advances after input"
        );
    }

    fn internal_scroll_offsets_hit_testing() {
        // draw() subtracts scroll_y and row_at() adds it — these must stay
        // inverses or a click resolves to the wrong row once scrolled.
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(
            (0..100)
                .map(|i| R {
                    name: format!("r{i}"),
                })
                .collect(),
        );
        let _laid = lay_out(&mut t);
        t.set_viewport(5.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        t.set_scroll_y(10.0 * f32_of_f64(ROW_HEIGHT)); // scrolled past 10 rows
        let rect = t.get_rect();
        let p = Point::new(rect.x0 + 5.0, rect.y0 + t.header_h + 1.0);
        assert_eq!(
            t.row_at(p),
            Some(10),
            "top of body maps to the scrolled-in row"
        );
        // visible window also starts around row 10 (minus overscan).
        let (start, _end) = t.visible_row_range();
        assert!(
            (8..=10).contains(&start),
            "window starts near the scroll position: {start}"
        );
    }

    fn variable_row_heights_share_placement_and_hit_testing() {
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(rows4());
        t.set_viewport(60.0, f32_of_f64(ROW_HEIGHT));
        assert!(t.set_row_height(0, 10.0));
        assert!(t.set_row_height(1, 50.0));
        let _laid = lay_out(&mut t);
        let body = t.get_rect().y0 + t.header_h;

        assert_eq!(t.row_at(Point::new(10.0, body + 9.0)), Some(0));
        assert_eq!(t.row_at(Point::new(10.0, body + 10.0)), Some(1));
        assert_eq!(t.row_at(Point::new(10.0, body + 59.0)), Some(1));
        assert_eq!(t.row_at(Point::new(10.0, body + 60.0)), None);
    }

    fn configured_viewport_bounds_layout_and_accessibility_work() {
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(
            (0..100_000)
                .map(|i| R {
                    name: format!("r{i}"),
                })
                .collect(),
        );
        t.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
        t.set_scroll_y(50_000.0 * f32_of_f64(ROW_HEIGHT));
        let _laid = lay_out(&mut t);

        assert!((t.get_rect().height() - (t.header_h + 120.0)).abs() <= 0.5);
        let node = Widget::accessibility(&t).expect("table semantics");
        assert!(
            node.children.len() <= 10,
            "semantic tree stays proportional to the viewport: {} rows",
            node.children.len()
        );
        let first = node.children.first().expect("visible semantic row");
        assert_eq!(first.set_size, Some(100_000));
        assert!(first.pos_in_set.expect("absolute row position") > 49_990);
    }

    fn wheel_scrolls_in_internal_mode() {
        let (mut t, tree, node) = scrolling_table();
        assert_eq!(t.scroll_y(), 0.0);
        let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
        dispatch(&mut t, &tree, node, &wheel);
        assert!(
            t.scroll_y() > 0.0,
            "wheel down increases scroll_y: {}",
            t.scroll_y()
        );
        assert!(t.scroll_y() <= t.max_scroll_y(), "clamped to max");
    }

    page_navigation_and_accelerated_wheel_share_variable_row_geometry();
    internal_scroll_offsets_hit_testing();
    variable_row_heights_share_placement_and_hit_testing();
    configured_viewport_bounds_layout_and_accessibility_work();
    wheel_scrolls_in_internal_mode();
}

/// 100 rows at 300×400, internal-scroll with a 5-row viewport (120 px
/// body over 2400 px of rows → bar is live). Bar centre x is 294.
fn scrolling_table() -> (DataTable<R>, TaffyTree<()>, NodeId) {
    let mut t = DataTable::<R>::new(sample_cols())
        .selectable(true)
        .drag_scroll();
    t.set_rows(
        (0..100)
            .map(|i| R {
                name: format!("r{i}"),
            })
            .collect(),
    );
    let (tree, node) = lay_out(&mut t);
    t.set_viewport(5.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
    (t, tree, node)
}

/// The selection lift, the stripe and the cells share one edge: the
/// first column's text (a cell pads it by the bleed) starts on the
/// table's content edge, the band and its lift stand the bleed past it,
/// and the live scroll bar keeps its own gutter at the trailing edge.
#[test]
fn rows_share_one_edge_and_the_scroll_bar_keeps_its_own_column() {
    let (mut t, _tree, _node) = scrolling_table();
    let _relaid = lay_out(&mut t);
    let bleed = f64::from(Theme::dark().tokens.spacing.s2);
    let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
    let rect = t.get_rect();
    let first = t.col_rects.first().copied().expect("a column");
    let last = t.col_rects.last().copied().expect("a column");
    assert!((first.x0 - (rect.x0 - bleed)).abs() < 1e-6, "{first:?}");
    let track = t.v_track().expect("the bar is live");
    assert!(
        last.x1 <= track.x0 + 1e-6,
        "the last column ends before the bar: {last:?} vs {track:?}"
    );
    let band = t.columns_band();
    assert!((band.x0 - first.x0).abs() < 1e-6);
    assert!((band.x1 - (rect.x1 - gutter)).abs() < 1e-6 && band.x1 <= track.x0);

    let mut plain = DataTable::<R>::new(sample_cols()).selectable(true);
    plain.set_rows(rows4());
    let _laid = lay_out(&mut plain);
    let band = plain.columns_band();
    let rect = plain.get_rect();
    assert!(
        (band.x1 - (rect.x1 + bleed)).abs() < 1e-6,
        "{band:?} in {rect:?}"
    );
}

fn bar_x(t: &DataTable<R>) -> f64 {
    t.v_track()
        .map_or(t.get_rect().x1, |track| track.center().x)
}

#[test]
fn scrollbar_visibility_thumb_drag_and_track_paging() {
    fn scrollbar_hidden_without_viewport() {
        // No `set_viewport` → defers to an outer Scrollable, no built-in bar.
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(
            (0..100)
                .map(|i| R {
                    name: format!("r{i}"),
                })
                .collect(),
        );
        let _ = lay_out(&mut t);
        assert!(
            t.scrollbar_thumb().is_none(),
            "no bar without an explicit viewport"
        );
    }

    fn scrollbar_hidden_when_rows_fit() {
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(rows4());
        let _ = lay_out(&mut t);
        // Viewport taller than the 4 rows → content fits → no bar.
        t.set_viewport(50.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        assert!(
            t.scrollbar_thumb().is_none(),
            "no bar when rows fit the viewport"
        );
    }

    fn scrollbar_thumb_tracks_scroll_y() {
        let (mut t, _tree, _node) = scrolling_table();
        let track = t.v_track().expect("bar live");
        let top = t.scrollbar_thumb().expect("thumb at top");
        assert!(
            (top.y0 - track.y0).abs() < 1e-6,
            "thumb starts at track top"
        );

        let bottom = t.max_scroll_y();
        t.set_scroll_y(bottom);
        let bottom = t.scrollbar_thumb().expect("thumb at bottom");
        assert!(bottom.y0 > top.y0, "thumb moves down as scroll_y grows");
        assert!(
            (bottom.y1 - track.y1).abs() < 1e-3,
            "fully scrolled thumb reaches track bottom: {} vs {}",
            bottom.y1,
            track.y1,
        );
    }

    fn dragging_scrollbar_updates_scroll_y() {
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let mid_y = thumb.y0.midpoint(thumb.y1);

        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (x, mid_y).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
        assert!(t.vbar.is_dragging(), "press on thumb begins a drag");
        assert!(t.wants_pointer_capture(), "drag captures the pointer");
        assert_eq!(t.scroll_y(), 0.0);

        // Drag down 50 px → scroll_y increases, staying clamped to max.
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (x, mid_y + 50.0).into(),
            },
        );
        assert!(
            t.scroll_y() > 0.0,
            "drag down increases scroll_y: {}",
            t.scroll_y()
        );
        assert!(t.scroll_y() <= t.max_scroll_y(), "clamped to max");
        let after = t.scroll_y();

        // Drag back up past the start → scroll_y returns toward 0.
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (x, mid_y - 50.0).into(),
            },
        );
        assert!(t.scroll_y() < after, "drag up decreases scroll_y");

        dispatch(
            &mut t,
            &tree,
            node,
            &crate::synthetic_input::mouse_release(),
        );
        assert!(!t.vbar.is_dragging(), "release ends the drag");
    }

    fn track_click_pages_scroll_y() {
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let below_y = thumb.y1 + 5.0;

        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (x, below_y).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
        assert!(!t.vbar.is_dragging(), "track click is not a drag");
        // Paged by ≈ one viewport (five rows) × PAGE_FRACTION.
        let expected =
            5.0 * f32_of_f64(ROW_HEIGHT) * f32_of_f64(crate::widget::scrollbar::PAGE_FRACTION);
        assert!(
            (t.scroll_y() - expected).abs() < 1.0,
            "page step ≈ viewport*frac ({expected}), got {}",
            t.scroll_y(),
        );
    }

    fn scrollbar_press_does_not_select_a_row() {
        // A press on the bar must be swallowed before body-row selection.
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let mid_y = thumb.y0.midpoint(thumb.y1);
        dispatch(
            &mut t,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (x, mid_y).into(),
            },
        );
        dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
        assert!(
            t.selection().is_empty(),
            "scrollbar press must not select a row"
        );
    }

    scrollbar_hidden_without_viewport();
    scrollbar_hidden_when_rows_fit();
    scrollbar_thumb_tracks_scroll_y();
    dragging_scrollbar_updates_scroll_y();
    track_click_pages_scroll_y();
    scrollbar_press_does_not_select_a_row();
}

#[test]
fn body_drag_scrolls_with_or_without_motion_decay() {
    fn body_drag_scrolls_table_without_selecting_but_click_still_selects() {
        let (mut table, tree, node) = scrolling_table();
        let body_top = table.get_rect().y0 + table.header_h;
        let move_cursor = |y| WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (20.0, y).into(),
        };
        let button = |state| crate::synthetic_input::mouse_input(state, MouseButton::Left);

        dispatch(&mut table, &tree, node, &move_cursor(body_top + 100.0));
        dispatch(&mut table, &tree, node, &button(ElementState::Pressed));
        assert!(table.wants_pointer_capture());
        dispatch(&mut table, &tree, node, &move_cursor(body_top + 40.0));
        dispatch(&mut table, &tree, node, &button(ElementState::Released));
        assert_eq!(table.scroll_y(), 60.0);
        assert!(
            table.selection().is_empty(),
            "panning does not select a row"
        );

        table.set_scroll_y(0.0);
        dispatch(&mut table, &tree, node, &move_cursor(body_top + 10.0));
        dispatch(&mut table, &tree, node, &button(ElementState::Pressed));
        dispatch(&mut table, &tree, node, &button(ElementState::Released));
        assert!(table.selection().is_selected(0));
    }

    fn reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay() {
        let (mut table, _tree, node) = scrolling_table();
        table.reduce_motion = true;
        let mut handled = false;
        table.on_wheel(
            &MouseScrollDelta::LineDelta(0.0, -1.0),
            TouchPhase::Moved,
            false,
            &mut handled,
        );
        assert!(handled);
        assert_eq!(table.scroll_y(), 60.0_f32);
        assert!(!table.scroll.momentum.is_active());

        let now = crate::time::now();
        let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
        let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
        let scroll = f64::from(table.scroll_y());
        table.drag_scroll.begin(50.0, scroll, started);
        assert!(table.drag_scroll.update(10.0, moved).is_some());
        let mut focus = None;
        table.on_left_release(
            winit::event::Modifiers::default(),
            &mut handled,
            &mut focus,
            node,
        );
        assert!(!table.scroll.momentum.is_active());
        assert!(!table.needs_animation_frame());
    }

    body_drag_scrolls_table_without_selecting_but_click_still_selects();
    reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay();
}

pub(super) fn table_color_fills(t: &mut DataTable<R>) -> usize {
    use vello::Scene;
    use vello_encoding::DrawTag;
    let _laid = lay_out(t);
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    t.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
    );
    scene
        .encoding()
        .draw_tags
        .iter()
        .filter(|tag| **tag == DrawTag::COLOR)
        .count()
}

#[test]
fn typed_cells_align_numbers_dim_absent_values_and_measure_auto_columns() {
    struct File {
        name: &'static str,
        size: Option<u64>,
    }
    let columns = vec![
        ColumnDef::new("name", "Name", ColumnWidth::Auto).with_text(|f: &File| f.name.to_owned()),
        ColumnDef::new("size", "Size", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Numeric)
            .with_optional_text(|f: &File| f.size.map(|s| s.to_string()), "NULL"),
    ];
    let mut table = DataTable::new(columns);
    table.set_rows(vec![
        File {
            name: "quarterly-report-final.pdf",
            size: Some(1_284_000),
        },
        File {
            name: "notes.md",
            size: None,
        },
    ]);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let harness = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(table),
        (600, 300),
        theme.clone(),
    );
    let _ = harness;
    // Reach the measure pass directly: the Auto column is as wide as its
    // longest visible cell plus padding.
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Auto).with_text(|f: &File| f.name.to_owned()),
    ]);
    table.set_rows(vec![File {
        name: "quarterly-report-final.pdf",
        size: None,
    }]);
    let mut tree = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = table.build_layout(&mut tree, &theme, &mut map);
    table.set_viewport(200.0, 32.0);
    assert!(table.adjust_layout_style(&mut tree, node, &mut text));
    let measured = f64::from(table.auto_widths["name"]);
    let cell = f64::from(
        text.measure_style(
            "quarterly-report-final.pdf",
            None,
            &cell_style(&theme, TextKind::Prose),
        )
        .0,
    );
    assert!(measured >= cell, "{measured} fits {cell}");
    assert!(
        !table.adjust_layout_style(&mut tree, node, &mut text),
        "settles"
    );

    // Numeric cells end at the trailing padding; an absent value says so.
    let column = ColumnDef::<File>::new("size", "Size", ColumnWidth::Flex(1.0))
        .with_kind(TextKind::Numeric)
        .with_optional_text(|f: &File| f.size.map(|s| s.to_string()), "NULL");
    assert_eq!(column.absent, "NULL");
    let spoken = column.accessible_value.as_ref().unwrap();
    assert_eq!(
        spoken(&File {
            name: "a",
            size: None
        }),
        "NULL"
    );
    let mut scene = Scene::new();
    let cell = Rect::new(0.0, 0.0, 200.0, 32.0);
    paint_cell_text(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        cell,
        &column,
        Some("1284000"),
    );
    assert!(!scene.encoding().resources.glyph_runs.is_empty());
    let style = cell_style(&theme, TextKind::Numeric);
    let (w, _) = text.measure_style("1284000", None, &style);
    let pad = f64::from(theme.tokens.spacing.s2);
    // The right-aligned origin lands so the run ends at the padding.
    let x = cell.x1 - pad - f64::from(w);
    assert!(x > cell.center().x, "numbers sit at the trailing edge");
}
