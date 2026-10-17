//! Integration: typing `/h2` then Enter on a focused paragraph block
//! must convert the block to Heading(2). Drives the `BlockEditor`'s
//! event chain end-to-end via the test hooks.
//!
//! See `crates/sdk/ui-core/src/widget/slash_menu.rs` and `block_editor.rs` for the
//! unit-level pins on each part of the flow.

use tilo_ui::widget::{BlockEditor, BlockKind, SlashCommand};
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey, SmolStr};

use tilo_ui::synthetic_input::key_press;

fn char_press(c: &str) -> WindowEvent {
    key_press(Key::Character(SmolStr::new(c)), Some(c))
}

#[test]
fn slash_menu_converts_the_focused_block_or_leaves_it_on_escape() {
    // Case `slash_h2_enter_converts_paragraph_to_heading2`.
    {
        let mut editor = BlockEditor::new();
        editor.set_focus(true);
        // Open via the test hook (mirrors the production `/` keystroke; avoids
        // building a TaffyTree just to drive the full WindowEvent chain).
        editor.open_slash_menu_for_test();
        assert!(editor.slash_menu().is_visible());

        // Filter to "h2" — the leading `/` is the editor's own responsibility;
        // the menu only sees the characters after.
        editor.slash_menu_handle_key_for_test(&char_press("h"));
        editor.slash_menu_handle_key_for_test(&char_press("2"));
        assert_eq!(
            editor.slash_menu().visible_len(),
            1,
            "filter `h2` should isolate Heading 2"
        );
        assert_eq!(
            editor.slash_menu().highlighted(),
            Some(&SlashCommand::Block(BlockKind::Heading(2))),
        );

        editor.slash_menu_handle_key_for_test(&key_press(Key::Named(NamedKey::Enter), None));
        assert!(!editor.slash_menu().is_visible());
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Heading(2)));
    }

    // Case `slash_menu_escape_leaves_block_unchanged`.
    {
        let mut editor = BlockEditor::new();
        editor.set_focus(true);
        editor.open_slash_menu_for_test();
        editor.slash_menu_handle_key_for_test(&char_press("h"));
        editor.slash_menu_handle_key_for_test(&key_press(Key::Named(NamedKey::Escape), None));
        assert!(!editor.slash_menu().is_visible());
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    }

    // Case `slash_menu_committed_kind_converts_focused_block`.
    {
        let mut editor = BlockEditor::new();
        editor.set_focus(true);
        // Pre-fill the block so we can verify text is preserved.
        editor.blocks_mut()[0].line.set_text("body");
        editor.blocks_mut()[0].line.set_cursor_pos(4);
        editor.open_slash_menu_for_test();
        // Pick "Bullet list" — type "bullet" then Enter.
        editor.slash_menu_handle_key_for_test(&char_press("b"));
        editor.slash_menu_handle_key_for_test(&char_press("u"));
        editor.slash_menu_handle_key_for_test(&char_press("l"));
        editor.slash_menu_handle_key_for_test(&key_press(Key::Named(NamedKey::Enter), None));
        match &editor.blocks()[0].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 0),
            k => panic!("expected Bullet, got {k:?}"),
        }
        assert_eq!(editor.blocks()[0].text(), "body");
    }
}
