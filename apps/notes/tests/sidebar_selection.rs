use crate::component_support::*;
use ui::headless::{FrameHarness, SemanticExpectation};

/// Whether the page tree's row of `page` is drawn selected, read from the
/// row's published semantics, which the row's painter reads too.
fn row_selected(driver: &FrameHarness, page: notes::PageId) -> bool {
    fn find(node: &ui::a11y::AccessNode, key: &str) -> Option<bool> {
        if node.semantic_key.as_deref() == Some(key) {
            return Some(node.state.selected);
        }
        node.children.iter().find_map(|child| find(child, key))
    }
    fn retained(node: &ui::headless::InspectionNode, key: &str) -> Option<bool> {
        node.semantics
            .as_ref()
            .and_then(|node| find(node, key))
            .or_else(|| node.children.iter().find_map(|node| retained(node, key)))
    }
    retained(&driver.inspection().root, &format!("notes:page:{page}"))
        .unwrap_or_else(|| panic!("the page tree shows a row for {page}"))
}

/// A window opens on its first note with nothing chosen yet; the open note
/// is the selected row in the sidebar from the first frame, and the mark
/// follows the note the window moves to.
#[test]
fn the_open_note_is_the_selected_row_in_the_sidebar() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut driver = driver(&f, 21, &checkout);
    driver
        .expect(
            &SemanticExpectation::named("note_title")
                .value("Projects")
                .within(std::time::Duration::from_secs(2)),
        )
        .unwrap();
    assert!(
        row_selected(&driver, f.root),
        "the note the window opened on is marked in the sidebar"
    );
    assert!(!row_selected(&driver, f.empty));

    open_note(&mut driver, 21, f.empty, None);
    driver
        .expect(
            &SemanticExpectation::named("note_title")
                .value("Empty")
                .within(std::time::Duration::from_secs(2)),
        )
        .unwrap();
    assert!(
        row_selected(&driver, f.empty),
        "the mark follows the open note"
    );
    assert!(!row_selected(&driver, f.root));
}
