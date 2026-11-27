//! A sidebar's list scrolls by the wheel when its rows run past the window.

use super::*;
use tilo_ui_app::widget::{NavItem, NavList, NavSection};

fn tall_list(rows: usize) -> NavList {
    NavList::new(vec![NavSection::untitled(
        "places",
        (0..rows)
            .map(|i| NavItem::new(format!("row-{i}"), format!("Row {i}")))
            .collect(),
    )])
}

/// The bounds the accessibility tree gives the row named `name`.
fn row_rect(h: &Driver, name: &str) -> Rect {
    let bounds = h
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .and_then(|(_, node)| node.bounds())
        .expect("the row is published with bounds");
    Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
}

#[test]
fn a_sidebar_taller_than_its_window_scrolls_by_wheel_to_its_last_row() {
    let shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(Box::new(tall_list(40)))
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Places", Vec::new())
        .content_header("Row 0", Vec::new());
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let sidebar = geometry(&h).test_body(Column::Sidebar);
    assert!(
        sidebar.height() > 0.0,
        "the window docks its sidebar: {sidebar:?}"
    );
    let last_before = row_rect(&h, "Row 39");
    assert!(
        last_before.y1 > sidebar.y1,
        "the last row waits past the sidebar's foot: {last_before:?} in {sidebar:?}"
    );

    let point = sidebar.center();
    h.dispatch_to_root(&tilo_ui_base::synthetic_input::cursor_moved(
        point.x, point.y,
    ));
    for _ in 0..12 {
        h.dispatch_to_root(&tilo_ui_base::synthetic_input::mouse_wheel(0.0, -4.0));
        h.relayout_at((1280, 800))
            .expect("the wheel's scroll lays out");
    }
    settle(&mut h, 1280, 800);
    let last = row_rect(&h, "Row 39");
    assert!(
        last.y0 >= sidebar.y0 && last.y1 <= sidebar.y1,
        "the last row is in the window: {last:?} in {sidebar:?}"
    );
}
