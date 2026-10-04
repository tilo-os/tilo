//! The notebook outlives the window: notes, their tree and their bodies are
//! the record's, and the note left open opens again.

use crate::framework_support::{add_note, apply, body, notes, open, surface};
use notes::PageId;
use notes::actions::new_page_action;

#[test]
fn a_reopened_notebook_keeps_its_tree_bodies_and_open_note() {
    let mut app = notes();
    let inbox = add_note(&mut app, "Inbox", "Call the dentist");
    let projects = add_note(&mut app, "Projects", "");
    let alpha = PageId::new_v4();
    apply(
        &mut app,
        &new_page_action(alpha, Some(projects), 0, "Project Alpha"),
    );
    open(&mut app, alpha);
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(alpha)
    );

    let mut app = app.reopen().unwrap();
    let pages = app.state().pages().to_vec();
    assert_eq!(pages.len(), 3);
    let title = |id| {
        pages
            .iter()
            .find(|page| page.id == id)
            .unwrap()
            .title
            .clone()
    };
    assert_eq!(title(inbox), "Inbox");
    assert_eq!(title(projects), "Projects");
    assert_eq!(title(alpha), "Project Alpha");
    assert_eq!(
        pages.iter().find(|page| page.id == alpha).unwrap().parent,
        Some(projects)
    );
    assert_eq!(
        app.state().children_of(None)[0].id,
        inbox,
        "sibling order holds"
    );
    assert_eq!(body(&app, inbox), ["Call the dentist"]);
    // The window shows the note it was left on.
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(alpha)
    );
}
