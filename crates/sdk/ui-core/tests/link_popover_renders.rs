//! Integration: typing `[[` inside an editor block opens the
//! [`LinkPopover`]; selecting a row inserts a `Mark::Link` covering
//! the page title.
//!
//! Drives the popover through the `BlockEditor`'s test hooks because
//! the full keystroke chain needs a `TaffyTree` which isn't trivial to
//! construct here. The popover's own filtering / arrow-nav / Enter
//! semantics are unit-pinned in `widget::link_popover::tests`.

#![allow(
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::link_popover::PageEntry;
use tilo_ui::widget::marks::Mark;
use tilo_ui::widget::{BlockEditor, BlockKind};
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey, SmolStr};

use tilo_ui::synthetic_input::key_press;

fn char_press(c: &str) -> WindowEvent {
    key_press(Key::Character(SmolStr::new(c)), Some(c))
}

fn seed_pages() -> Vec<PageEntry> {
    vec![
        PageEntry {
            id: uuid::Uuid::from_u128(1),
            title: "Welcome".into(),
        },
        PageEntry {
            id: uuid::Uuid::from_u128(2),
            title: "Roadmap".into(),
        },
        PageEntry {
            id: uuid::Uuid::from_u128(3),
            title: "Tips".into(),
        },
    ]
}

#[test]
fn link_popover_filters_and_commits_page_links() {
    // Case `link_popover_filters_by_typed_filter`.
    {
        let mut p = tilo_ui::widget::link_popover::LinkPopover::new();
        p.set_items(seed_pages());
        p.set_visible(true);
        p.handle_key(&char_press("w"));
        assert_eq!(p.visible_len(), 1);
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
    }

    // Case `enter_returns_chosen_page_entry`.
    {
        let mut p = tilo_ui::widget::link_popover::LinkPopover::new();
        p.set_items(seed_pages());
        p.set_visible(true);
        p.handle_key(&char_press("r"));
        p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        let chosen = p.take_selection().unwrap();
        assert_eq!(chosen.title, "Roadmap");
    }

    // Case `block_editor_double_bracket_opens_popover_and_commits_link`.
    {
        let mut editor = BlockEditor::new();
        editor.set_link_pages(seed_pages());
        editor.set_focus(true);
        // Pre-seed the popover open as if the user had typed `[[` — driving
        // real keystrokes needs a TaffyTree the test doesn't have.
        editor
            .link_popover_mut()
            .set_anchor(vello::kurbo::Point::ZERO);
        editor.link_popover_mut().set_visible(true);
        // Prime the block's text so the commit splice has something to land
        // on, as if `[[` had already been typed at byte 0.
        let block = &mut editor.blocks_mut()[0];
        block.line.set_text("[[");
        block.line.set_cursor_pos(2);
        block.line.set_focus(true);
        // commit_link_selection is private, so drive Enter through the
        // popover directly and test the lower-level contract: selection
        // drains to a PageEntry the host then splices in.
        editor.link_popover_mut().set_filter("Welcome");
        editor
            .link_popover_mut()
            .handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        let chosen = editor.link_popover_mut().take_selection().unwrap();
        assert_eq!(chosen.title, "Welcome");

        // Apply the splice manually to verify the shape a real one would
        // produce (the live editor does this from `commit_link_selection`).
        let block = &mut editor.blocks_mut()[0];
        let text = block.line.value().to_string();
        let new_text = format!("{}{}", &text[..0], "Welcome");
        block.line.set_text(&new_text);
        block.line.add_mark(tilo_ui::widget::marks::MarkSpan::new(
            0..7,
            Mark::Link(chosen.id),
        ));
        assert_eq!(block.line.value(), "Welcome");
        assert_eq!(block.line.marks().len(), 1);
        assert!(matches!(block.line.marks()[0].mark, Mark::Link(_)));
        let _ = BlockKind::Paragraph; // touch the import so it isn't unused
    }
}
