use quick_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

#[derive(Clone)]
struct Row {
    name: String,
    size: u64,
}

fn cols() -> Vec<ColumnDef<Row>> {
    vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t| {})),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(80.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t| {})),
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
