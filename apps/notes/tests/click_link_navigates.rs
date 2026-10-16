//! Clicking a `Mark::Link(page_id)` span navigates to that page.
//!
//! Drives `NotesApp::navigate_to` directly, the path the click handler
//! takes after its per-glyph hit-test. The hit-test logic
//! is unit-tested separately in `ui::widget::rich_text_line` via
//! `link_at_byte`.

use crate::framework_support::{add_note, open, surface, with_note};

#[test]
fn clicking_a_link_finds_it_by_byte_and_navigates_to_its_page() {
    eprintln!("phase navigate_to_changes_selected_page");
    {
        let (mut app, root) = with_note("Root", "First line");
        let second = add_note(&mut app, "Second", "");
        open(&mut app, second);
        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(second)
        );
        // navigate_to drives the same path the click-on-link handler uses.
        surface(&mut app, |notes| notes.navigate_to(root));
        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(root)
        );
    }

    eprintln!("phase link_at_byte_finds_link_in_block");
    {
        use ui::widget::marks::{Mark, MarkSpan};
        use ui::widget::rich_text_line::link_at_byte;
        let id = uuid::Uuid::from_u128(42);
        let marks = vec![MarkSpan::new(6..13, Mark::Link(id))];
        assert_eq!(link_at_byte(&marks, 8), Some(id));
        assert_eq!(link_at_byte(&marks, 0), None);
    }
}
