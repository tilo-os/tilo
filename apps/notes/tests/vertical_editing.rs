//! Production keyboard routing across shaped rows and block boundaries.
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::{BlockEditor, CursorPlacement, Widget};
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

struct Editor {
    editor: Framed<BlockEditor>,
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn vertical_navigation_and_selection_keep_anchor_column_and_content() {
    eprintln!("phase arrow_navigation_never_emits_content_edits");
    {
        let mut fixture = Editor::new(
            "A paragraph long enough to wrap across several visual rows at this narrow width.\n\nAnother paragraph long enough to wrap as well.",
            155.0,
            0,
            3,
        );
        fixture.editor.take_structure_dirty();
        fixture.editor.is_dirty();
        for shift in [false, true] {
            for key in [NamedKey::ArrowDown, NamedKey::ArrowDown, NamedKey::ArrowUp] {
                fixture.key(key, shift);
                assert!(fixture.editor.take_edit_ops().is_empty());
                assert!(
                    !fixture.editor.is_dirty(),
                    "{key:?} must not report a content edit"
                );
            }
        }
    }

    eprintln!("phase shift_arrows_keep_the_original_anchor_across_three_blocks_and_reverse");
    {
        let mut fixture = Editor::new("first\n\nsecond\n\nthird", 400.0, 0, 0);
        fixture.key(NamedKey::ArrowDown, true);
        fixture.key(NamedKey::ArrowDown, true);
        assert_eq!(
            fixture.editor.selected_text().as_deref(),
            Some("first\nsecond\n")
        );
        let selection = fixture.editor.document_selection().unwrap();
        assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 0));
        assert_eq!((selection.cursor.block, selection.cursor.byte), (2, 0));
        fixture.key(NamedKey::ArrowUp, true);
        assert_eq!(fixture.editor.selected_text().as_deref(), Some("first\n"));
        fixture.key(NamedKey::ArrowUp, true);
        assert!(fixture.editor.document_selection().is_none());
    }

    eprintln!("phase wrapped_shift_navigation_and_delete_use_the_same_document_range");
    {
        let first = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let second = "one two three four five six seven eight nine ten eleven twelve thirteen";
        let mut fixture = Editor::new(&format!("{first}\n\n{second}"), 155.0, 0, 3);
        for _ in 0..20 {
            if fixture.editor.focused_index() == Some(1) {
                break;
            }
            fixture.key(NamedKey::ArrowDown, true);
        }
        let boundary = fixture.editor.document_selection().unwrap();
        assert_eq!(boundary.cursor.block, 1, "must reach the next paragraph");
        assert_eq!((boundary.anchor.block, boundary.anchor.byte), (0, 3));
        fixture.key(NamedKey::ArrowDown, true);
        let selection = fixture.editor.document_selection().unwrap();
        assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 3));
        assert_eq!(selection.cursor.block, 1);
        assert!(
            selection.cursor.byte > boundary.cursor.byte,
            "selection must advance inside the wrapped second paragraph"
        );
        assert_eq!(
            fixture.editor.selected_text().unwrap(),
            format!("{}\n{}", &first[3..], &second[..selection.cursor.byte])
        );
        fixture.key(NamedKey::Delete, false);
        assert_eq!(fixture.editor.blocks().len(), 1);
        assert_eq!(
            fixture.editor.blocks()[0].text(),
            format!("{}{}", &first[..3], &second[selection.cursor.byte..])
        );
    }

    eprintln!("phase vertical_column_survives_a_short_block_in_both_directions");
    {
        let text = "abcdefghijklmnopqrstuv";
        let mut fixture = Editor::new(&format!("{text}\n\nx\n\n{text}"), 400.0, 0, 15);
        fixture.key(NamedKey::ArrowDown, false);
        assert_eq!(fixture.editor.focused_index(), Some(1));
        fixture.key(NamedKey::ArrowDown, false);
        assert_eq!(fixture.editor.focused_index(), Some(2));
        assert_eq!(fixture.editor.blocks()[2].line.cursor_pos(), 15);
        fixture.key(NamedKey::ArrowUp, false);
        fixture.key(NamedKey::ArrowUp, false);
        assert_eq!(fixture.editor.focused_index(), Some(0));
        assert_eq!(fixture.editor.blocks()[0].line.cursor_pos(), 15);
    }

    eprintln!("phase extending_an_existing_local_selection_preserves_its_anchor");
    {
        let mut fixture = Editor::new("first\n\nsecond\n\nthird", 400.0, 0, 4);
        fixture.editor.blocks_mut()[0]
            .line
            .inner_mut()
            .set_selection(1, 4);
        fixture.key(NamedKey::ArrowDown, true);
        let selection = fixture.editor.document_selection().unwrap();
        assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 1));
        assert_eq!(selection.cursor.block, 1);
        assert_eq!(
            fixture.editor.selected_text().unwrap(),
            format!("irst\n{}", &"second"[..selection.cursor.byte])
        );
    }
}

impl Editor {
    fn new(markdown: &str, width: f32, block: usize, byte: usize) -> Self {
        let mut editor = crate::component_support::framed_block_editor(
            BlockEditor::from_markdown(markdown).with_wrap_lines(true),
            width,
            (50.0, 50.0),
            Theme::dark(),
        );
        editor.set_focus(true);
        editor.focus_block(block, CursorPlacement::Byte(byte));
        Self { editor }
    }

    /// Run the frame the last input left pending, then press `key`.
    fn key(&mut self, key: NamedKey, shift: bool) {
        // winit keeps platform fields private; this matches the existing
        // headless keyboard fixtures used throughout the integration suite.
        let event = ui::synthetic_input::key_event(Key::Named(key), None, ElementState::Pressed);
        let event = ui::synthetic_input::keyboard_input(event);
        let modifiers = Modifiers::from(if shift {
            ModifiersState::SHIFT
        } else {
            ModifiersState::empty()
        });
        let harness = Framed::harness_mut(&mut self.editor);
        harness.relayout();
        let handled = harness.dispatch_to_root_with(&event, &modifiers);
        assert!(handled, "editor must consume {key:?}");
    }
}
