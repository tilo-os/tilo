use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

#[derive(Clone)]
struct Row {
    name: String,
    #[allow(dead_code)] // fixture column; only `name` is read in current assertions
    size: u64,
}

fn cols() -> Vec<ColumnDef<Row>> {
    vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).sortable(true),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(80.0)).sortable(true),
    ]
}

#[test]
fn data_table_sorts_ascending_and_descending() {
    let mut t = DataTable::new(cols());
    t.set_rows(vec![
        Row { name: "c".into(), size: 30 },
        Row { name: "a".into(), size: 10 },
        Row { name: "b".into(), size: 20 },
    ]);
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
    let v = t.view_rows();
    assert_eq!(v[0].name, "a");
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", false);
    let v = t.view_rows();
    assert_eq!(v[0].name, "c");
}

#[test]
fn data_table_column_resize_updates_width() {
    let mut t = DataTable::new(cols());
    assert_eq!(t.column_width_px("size"), Some(80.0));
    t.begin_resize("size", 80.0);
    t.drag_resize(95.0); // delta = +15
    t.end_resize();
    assert_eq!(t.column_width_px("size"), Some(95.0));
    // clamped to >= min
    t.begin_resize("size", 95.0);
    t.drag_resize(10.0);
    t.end_resize();
    assert_eq!(t.column_width_px("size"), Some(DataTable::<Row>::MIN_COL_WIDTH));
}

#[test]
fn data_table_sort_by_flips_direction_when_comparator_registered() {
    // `sort_by(column_key, ascending)` is the prompt-locked public entry; it
    // reapplies the existing comparator with the requested direction. It
    // returns `true` when the column has a comparator installed (via a prior
    // `sort_by_with`), `false` otherwise.
    let mut t = DataTable::new(cols());
    t.set_rows(vec![
        Row { name: "c".into(), size: 30 },
        Row { name: "a".into(), size: 10 },
        Row { name: "b".into(), size: 20 },
    ]);
    // No comparator yet → sort_by is a no-op, reports `false`.
    assert!(!t.sort_by("name", true));
    // Install via the typed helper, then use sort_by to flip direction.
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
    assert!(t.sort_by("name", false));
    assert_eq!(t.view_rows()[0].name, "c");
    assert!(t.sort_by("name", true));
    assert_eq!(t.view_rows()[0].name, "a");
    // Unknown column key → false, state unchanged.
    assert!(!t.sort_by("size", true));
    assert_eq!(t.view_rows()[0].name, "a");
}

#[test]
fn data_table_virtualization_windows_rows() {
    let mut t = DataTable::new(cols());
    let rows: Vec<Row> = (0..10_000).map(|i| Row { name: format!("r{i}"), size: i as u64 }).collect();
    t.set_rows(rows);
    t.set_viewport(500.0, 24.0);        // 500px tall, 24px row height
    t.set_scroll_y(4800.0);             // scrolled down 200 rows
    let (start, end) = t.visible_row_range();
    assert!(start >= 195 && start <= 205, "start={start}");
    // (viewport / row_height) + small overscan
    assert!(end - start >= 20 && end - start <= 32, "range size = {}", end - start);
}
