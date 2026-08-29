use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

#[derive(Clone)]
struct Row {
    name: String,
    #[allow(dead_code)] // fixture column; only `name` is read in current assertions
    size: u64,
}

fn cols() -> Vec<ColumnDef<Row>> {
    vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t, _tr, _rect| {})),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(80.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t, _tr, _rect| {})),
    ]
}

#[test]
fn data_table_sorts_ascending_and_descending() {
    let mut t = DataTable::new(cols());
    t.set_rows(vec![
        Row {
            name: "c".into(),
            size: 30,
        },
        Row {
            name: "a".into(),
            size: 10,
        },
        Row {
            name: "b".into(),
            size: 20,
        },
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
    t.drag_resize(95.0);
    t.end_resize();
    assert_eq!(t.column_width_px("size"), Some(95.0));
    // clamped to >= min
    t.begin_resize("size", 95.0);
    t.drag_resize(10.0);
    t.end_resize();
    assert_eq!(
        t.column_width_px("size"),
        Some(DataTable::<Row>::MIN_COL_WIDTH)
    );
}

#[test]
fn data_table_sort_by_flips_direction_when_comparator_registered() {
    // `sort_by` reapplies the existing comparator with a new direction;
    // `true` only if the column already has one installed.
    let mut t = DataTable::new(cols());
    t.set_rows(vec![
        Row {
            name: "c".into(),
            size: 30,
        },
        Row {
            name: "a".into(),
            size: 10,
        },
        Row {
            name: "b".into(),
            size: 20,
        },
    ]);
    // No comparator yet.
    assert!(!t.sort_by("name", true));
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
    assert!(t.sort_by("name", false));
    assert_eq!(t.view_rows()[0].name, "c");
    assert!(t.sort_by("name", true));
    assert_eq!(t.view_rows()[0].name, "a");
    // Unknown column key.
    assert!(!t.sort_by("size", true));
    assert_eq!(t.view_rows()[0].name, "a");
}

/// The cell-render closure receives the shared `TextRenderer` and a
/// non-degenerate cell `Rect` on the production `draw` path, and drawing
/// real text through it lands a glyph run in the scene. Proves the new
/// 5-arg `CellRender` signature is wired through `DataTable::draw`, not
/// just type-checked.
#[test]
fn cell_render_receives_text_renderer_and_rect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_nonzero_rect = Arc::new(AtomicUsize::new(0));

    let calls_c = calls.clone();
    let rect_c = saw_nonzero_rect.clone();
    let columns = vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).with_render(Box::new(
            move |r: &Row, scene: &mut Scene, theme: &Theme, tr: &mut TextRenderer, rect: Rect| {
                calls_c.fetch_add(1, Ordering::SeqCst);
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    rect_c.fetch_add(1, Ordering::SeqCst);
                }
                tr.draw_text(
                    scene,
                    &r.name,
                    12.0,
                    theme.text_primary,
                    Affine::translate((rect.x0 + 4.0, rect.y0)),
                    None,
                );
            },
        )),
    ];

    let mut t = DataTable::new(columns);
    t.set_rows(vec![
        Row {
            name: "alpha".into(),
            size: 1,
        },
        Row {
            name: "beta".into(),
            size: 2,
        },
    ]);

    // Populate `col_rects` and `rect` before drawing through the real path.
    let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
    let mut node_map = std::collections::HashMap::new();
    let theme = Theme::dark();
    let root = t.build_layout(&mut taffy, &theme, &mut node_map);
    // Give the table an explicit size so columns resolve to non-zero widths.
    taffy
        .set_style(
            root,
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(400.0),
                    height: taffy::Dimension::length(200.0),
                },
                ..Default::default()
            },
        )
        .unwrap();
    taffy
        .compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(200.0),
            },
        )
        .unwrap();
    t.apply_layout(&taffy, root, Point::ZERO);

    let mut tr = TextRenderer::new();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = Scene::new();
    Widget::draw(
        &mut t,
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
    );

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "cell render runs once per visible row"
    );
    assert_eq!(
        saw_nonzero_rect.load(Ordering::SeqCst),
        2,
        "each cell render must receive a non-degenerate cell rect",
    );
    assert!(
        !scene.encoding().resources.glyph_runs.is_empty(),
        "text drawn through the cell TextRenderer must land in the scene",
    );
}

#[test]
fn data_table_virtualization_windows_rows() {
    let mut t = DataTable::new(cols());
    let rows: Vec<Row> = (0..10_000)
        .map(|i| Row {
            name: format!("r{i}"),
            size: i as u64,
        })
        .collect();
    t.set_rows(rows);
    t.set_viewport(500.0, 24.0); // 500px tall, 24px row height
    t.set_scroll_y(4800.0); // scrolled down 200 rows
    let (start, end) = t.visible_row_range();
    assert!((195..=205).contains(&start), "start={start}");
    // (viewport / row_height) + small overscan
    assert!(
        end - start >= 20 && end - start <= 32,
        "range size = {}",
        end - start
    );
}
