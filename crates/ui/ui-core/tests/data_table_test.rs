use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

#[derive(Clone)]
struct Row {
    name: String,
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
            move |r: &Row,
                  scene: &mut Scene,
                  transform: Affine,
                  theme: &Theme,
                  tr: &mut TextRenderer,
                  rect: Rect| {
                calls_c.fetch_add(1, Ordering::SeqCst);
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    rect_c.fetch_add(1, Ordering::SeqCst);
                }
                tr.draw_text(
                    scene,
                    &r.name,
                    12.0,
                    theme.text_primary,
                    transform * Affine::translate((rect.x0 + 4.0, rect.y0)),
                    None,
                );
            },
        )),
    ];

    let mut t = DataTable::new(columns);
    t.set_rows(vec![
        Row {
            name: "alpha".into(),
        },
        Row {
            name: "beta".into(),
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
                size: taffy::Size::from_lengths(400.0, 200.0),
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
    let presence = tilo_ui::presence::PresenceCtx::default();
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
