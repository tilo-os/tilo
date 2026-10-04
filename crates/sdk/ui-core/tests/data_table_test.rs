#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

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

/// The same visible header reaches a host-owned sort callback from pointing,
/// accessibility activation and keyboard focus; rows retain their stable ids.
#[test]
fn sortable_headers_are_operable_without_changing_row_targets() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_ui::a11y::{Action, Role};
    use tilo_ui::headless::Framed;

    let requests = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&requests);
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text(|row: &Row| row.name.clone()),
        ColumnDef::new("pid", "PID", ColumnWidth::Fixed(80.0)).sortable(true),
        ColumnDef::new("detail", "Detail", ColumnWidth::Fixed(80.0)),
    ])
    .with_id("monitor.processes")
    .with_accessible_name("Processes")
    .with_row_key(|row: &Row| u64::from(row.name.as_bytes()[0]))
    .selectable(true)
    .on_header_click(move |column| received.borrow_mut().push(column));
    table.set_rows(vec![
        Row {
            name: "beta".into(),
        },
        Row {
            name: "alpha".into(),
        },
    ]);
    let mut framed = Framed::new(tilo_ui::State::new(), table, (480, 240), Theme::dark());
    let before = Widget::accessibility(&*framed).unwrap();
    let row_targets: Vec<_> = before
        .children
        .iter()
        .filter(|node| node.role == Role::Row)
        .map(|node| (node.key, node.action_target.clone()))
        .collect();
    let harness = Framed::harness_mut(&mut framed);
    let name = harness
        .a11y_node_named("Sort by name")
        .expect("header button");
    let nodes = harness.a11y_nodes();
    let bounds = nodes
        .iter()
        .find(|(id, _)| *id == name)
        .unwrap()
        .1
        .bounds()
        .unwrap();
    assert!(bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0);
    harness.click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    assert_eq!(*requests.borrow(), [0]);
    let name = harness.a11y_node_named("Sort by name").unwrap();
    harness
        .perform_a11y_action(name, &Action::Activate)
        .unwrap();
    assert_eq!(*requests.borrow(), [0, 0]);
    harness.perform_a11y_action(name, &Action::Focus).unwrap();
    harness.key("Enter");
    assert_eq!(*requests.borrow(), [0, 0, 0]);
    harness.key("ArrowRight");
    harness.key("Enter");
    assert_eq!(*requests.borrow(), [0, 0, 0, 1]);
    harness.key("ArrowDown");
    harness.key("F6");
    harness.key("Enter");
    assert_eq!(*requests.borrow(), [0, 0, 0, 1, 0]);
    assert!(harness.a11y_node_named("Sort by detail").is_none());
    assert_eq!(
        framed.sort_state(),
        None,
        "the host callback still owns sort order"
    );
    let after = Widget::accessibility(&*framed).unwrap();
    let after_targets: Vec<_> = after
        .children
        .iter()
        .filter(|node| node.role == Role::Row)
        .map(|node| (node.key, node.action_target.clone()))
        .collect();
    assert_eq!(
        after_targets, row_targets,
        "synthetic headers never renumber row identities"
    );
}

#[test]
fn semantic_sort_uses_the_registered_comparator_and_updates_direction() {
    use tilo_ui::a11y::Action;
    use tilo_ui::headless::Framed;
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text(|row: &Row| row.name.clone()),
    ]);
    table.set_rows(vec![
        Row {
            name: "beta".into(),
        },
        Row {
            name: "alpha".into(),
        },
    ]);
    table.sort_by_with(|left, right| left.name.cmp(&right.name), "name", true);
    let mut framed = Framed::new(tilo_ui::State::new(), table, (320, 180), Theme::dark());
    let harness = Framed::harness_mut(&mut framed);
    let header = harness.a11y_node_named("Sort by name").unwrap();
    harness
        .perform_a11y_action(header, &Action::Activate)
        .unwrap();
    assert_eq!(framed.sort_state(), Some(("name", false)));
    assert_eq!(
        framed
            .view_rows()
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["beta", "alpha"]
    );
    let tree = Widget::accessibility(&*framed).unwrap();
    let header = tree
        .children
        .iter()
        .flat_map(|node| &node.children)
        .find(|node| node.action_target.as_deref() == Some("column:name"))
        .unwrap();
    assert_eq!(header.description.as_deref(), Some("Sorted descending"));
}

/// A row without an explicit identity uses the native key-path action route.
/// Leaving a header through that route must return keyboard ownership to rows.
#[test]
fn unkeyed_row_focus_leaves_sort_header_and_enter_activates_row() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_ui::a11y::Action;
    use tilo_ui::headless::Framed;

    let sorts = Rc::new(RefCell::new(Vec::new()));
    let activations = Rc::new(RefCell::new(Vec::new()));
    let sorted = Rc::clone(&sorts);
    let activated = Rc::clone(&activations);
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text(|row: &Row| row.name.clone()),
    ])
    .selectable(true)
    .on_header_click(move |column| sorted.borrow_mut().push(column))
    .on_activate(move |row| activated.borrow_mut().push(row));
    table.set_rows(vec![
        Row {
            name: "alpha".into(),
        },
        Row {
            name: "beta".into(),
        },
    ]);
    let mut framed = Framed::new(tilo_ui::State::new(), table, (320, 180), Theme::dark());
    let harness = Framed::harness_mut(&mut framed);
    let header = harness.a11y_node_named("Sort by name").unwrap();
    harness.perform_a11y_action(header, &Action::Focus).unwrap();
    harness.key("Enter");
    assert_eq!(*sorts.borrow(), [0]);
    let row = harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == accesskit::Role::Row)
        .nth(1)
        .unwrap()
        .0;
    harness.perform_a11y_action(row, &Action::Focus).unwrap();
    assert_eq!(harness.a11y_focus().unwrap().0, row);
    harness.key("Enter");
    assert_eq!(*activations.borrow(), [1]);
    assert_eq!(
        *sorts.borrow(),
        [0],
        "row Enter must not sort the old header"
    );
    harness.perform_a11y_action(header, &Action::Focus).unwrap();
    harness.perform_a11y_action(row, &Action::Activate).unwrap();
    assert_eq!(harness.a11y_focus().unwrap().0, row);
    harness.key("Enter");
    assert_eq!(*activations.borrow(), [1, 1, 1]);
    assert_eq!(*sorts.borrow(), [0]);
}
