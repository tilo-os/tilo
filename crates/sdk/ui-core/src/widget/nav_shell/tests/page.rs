//! The shell on a page: its columns and header rows stand where the shell laid them out.

use super::*;

fn found<'a>(
    node: &'a crate::composition::Composed,
    path: &str,
) -> Option<&'a crate::composition::Composed> {
    if node.path.ends_with(path) {
        return Some(node);
    }
    node.children.iter().find_map(|child| found(child, path))
}

/// A press on the page lands on what the shell laid out under it, so a header row stands as tall
/// as the shell made it, whatever it holds, and the rows under it stand where they are pressed.
#[test]
fn a_pages_columns_stand_where_the_shell_laid_them_out() {
    let shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(Box::new(crate::widget::Button::new("one", "One")))
        .content(Box::new(crate::widget::Button::new("two", "Two")))
        .sidebar_header("Places", Vec::new())
        .content_header("A page", Vec::new());
    let mut harness = FrameHarness::for_root(State::new(), shell, (1280, 800), Theme::light());
    for _ in 0..3 {
        harness.relayout();
    }
    let geometry = harness.root::<NavShell>().geometry.clone();
    let composition = harness.composition();
    let rect = |r: Rect| [r.x0, r.y0, r.width(), r.height()];
    for (path, expected) in [
        ("#sidebar", geometry.sidebar_column),
        ("#sidebar-header", geometry.header(Column::Sidebar)),
        ("#content", geometry.content_column),
        ("#content-header", geometry.header(Column::Content)),
    ] {
        let region = found(&composition.root, path).expect("the shell composes the region");
        assert_eq!(
            region.rect,
            rect(expected),
            "{path} stands where the shell put it"
        );
    }
    let body = found(&composition.root, "#sidebar")
        .and_then(|column| column.children.get(1))
        .and_then(|body| body.layout.clone())
        .unwrap_or_default();
    assert_eq!(
        body.matches("flex:1;").count(),
        1,
        "a body restyled each frame keeps one of each declaration: {body}"
    );
}
