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
