//! Headings made from the format menu and its chords: writing that is already there takes the line's
//! kind, its text and marks stay, and body text is the way back.

use super::*;
use automerge::ROOT;
use ui::testing::Dispatch;
use winit::keyboard::{Key, ModifiersState};

fn chord(
    editor: &mut BlockEditor,
    doc: &mut AutoCommit,
    modifiers: ModifiersState,
    key: &str,
) -> bool {
    let event = tilo_ui_base::synthetic_input::key_event(
        Key::Character(key.into()),
        None,
        ElementState::Pressed,
    );
    let handled = Dispatch::new()
        .modifiers(modifiers)
        .doc(doc)
        .send(
            editor,
            &tilo_ui_base::synthetic_input::keyboard_input(event),
        )
        .handled;
    editor.sync(doc, &ROOT);
    handled
}

/// An editor focused on the first of `texts`, each a paragraph as written.
fn editing(texts: &[&str]) -> BlockEditor {
    let blocks: Vec<BlockContent> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| BlockContent::paragraph(format!("b{index}"), *text))
        .collect();
    let mut editor = BlockEditor::new();
    editor.present_blocks(&blocks);
    editor.set_focus(true);
    editor.focus_block(0, CursorPlacement::End);
    editor
}

fn kinds(editor: &BlockEditor) -> Vec<BlockKind> {
    editor
        .blocks()
        .iter()
        .map(|block| block.kind.clone())
        .collect()
}

fn texts(editor: &BlockEditor) -> Vec<String> {
    editor
        .blocks()
        .iter()
        .map(|block| block.text().to_owned())
        .collect()
}

fn activate_row(editor: &mut BlockEditor, doc: &mut AutoCommit, name: &str) {
    fn find<'a>(
        node: &'a tilo_ui_base::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a tilo_ui_base::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    let tree = Widget::accessibility(editor).expect("the editor is in the tree");
    let target = find(&tree, name)
        .unwrap_or_else(|| panic!("the menu offers {name}"))
        .action_target
        .clone()
        .expect("a row is actionable");
    assert!(
        editor.perform_semantic_action(
            &target,
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(doc),
            &ROOT,
        ),
        "the row {name} runs"
    );
    editor.sync(doc, &ROOT);
}

tilo_test_env::scenario! {
    /// Headings from the format menu and the chords, their style and their edit.
    /// Each phase was a test of its own and builds its own fixture.
    fn headings_are_made_drawn_and_written_as_edits;

    phase the_format_menu_makes_a_line_a_heading_and_body_text_again;
    {
        let (services, _recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        let mut doc = AutoCommit::new();
        let mut editor = editing(&["Plan"]);
        editor.blocks[0]
            .line
            .apply_layout_external(Rect::new(0.0, 0.0, 300.0, 24.0));
        assert!(editor.open_text_menu(Point::new(10.0, 12.0)));
        assert!(
            editor.text_menu().is_some(),
            "a secondary press on the words opens the menu"
        );

        activate_row(&mut editor, &mut doc, "Heading 2");
        assert_eq!(kinds(&editor), [BlockKind::Heading(2)]);
        assert_eq!(texts(&editor), ["Plan"], "the words stay as they were");
        assert!(editor.text_menu().is_none(), "a chosen row closes the menu");

        assert!(editor.open_text_menu(Point::new(10.0, 12.0)));
        activate_row(&mut editor, &mut doc, "Body text");
        assert_eq!(kinds(&editor), [BlockKind::Paragraph]);
        assert_eq!(texts(&editor), ["Plan"]);

        assert!(editor.open_text_menu(Point::new(10.0, 12.0)));
        activate_row(&mut editor, &mut doc, "Heading 1");
        assert_eq!(kinds(&editor), [BlockKind::Heading(1)]);
    }

    phase a_heading_is_drawn_in_the_toolkit_heading_style;
    {
        let mut doc = AutoCommit::new();
        let mut editor = editing(&["Plan"]);
        chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "1",
        );
        let heading = editor.blocks()[0].kind.clone();
        assert_eq!(heading, BlockKind::Heading(1));
        assert_eq!(
            heading.text_role(),
            TextRole::LargeTitle,
            "a first-level heading takes the toolkit's title role"
        );
        assert_eq!(BlockKind::Heading(2).text_role(), TextRole::Title);
        assert_eq!(BlockKind::Heading(3).text_role(), TextRole::Heading);
        assert!(
            heading.font_size() > BlockKind::Paragraph.font_size()
                && BlockKind::Heading(2).font_size() > BlockKind::Paragraph.font_size()
                && BlockKind::Heading(3).font_size() > BlockKind::Paragraph.font_size(),
            "every heading is set larger than body text"
        );
    }

    phase the_chords_make_the_line_a_heading_of_their_level_and_body_text_again;
    {
        let mut doc = AutoCommit::new();
        let mut editor = editing(&["Plan", "Groceries"]);
        for (key, level) in [("2", 2), ("3", 3), ("1", 1)] {
            assert!(
                chord(
                    &mut editor,
                    &mut doc,
                    ModifiersState::CONTROL | ModifiersState::ALT,
                    key
                ),
                "Ctrl+Alt+{key} is the editor's"
            );
            assert_eq!(kinds(&editor)[0], BlockKind::Heading(level));
            assert_eq!(texts(&editor)[1], "Groceries", "the line below is its own");
        }
        assert!(chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "0"
        ));
        assert_eq!(kinds(&editor)[0], BlockKind::Paragraph);
        assert_eq!(texts(&editor)[0], "Plan", "the words never moved");
    }

    phase the_chord_makes_every_selected_paragraph_a_heading;
    {
        let mut doc = AutoCommit::new();
        let mut editor = editing(&["Plan", "Groceries"]);
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition { block: 1, byte: 9 },
        );
        assert!(chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "2"
        ));
        assert_eq!(
            kinds(&editor),
            [BlockKind::Heading(2), BlockKind::Heading(2)],
            "every paragraph the selection spans"
        );
        assert_eq!(texts(&editor), ["Plan", "Groceries"]);
    }

    phase a_code_block_keeps_its_kind_when_the_chord_is_pressed;
    {
        let mut doc = AutoCommit::new();
        let mut blocks = vec![BlockContent::paragraph("b0", "fn main() {}")];
        blocks[0].kind = BlockKind::CodeBlock { lang: None };
        let mut editor = BlockEditor::new();
        editor.present_blocks(&blocks);
        editor.set_focus(true);
        editor.focus_block(0, CursorPlacement::End);
        assert!(chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "1"
        ));
        assert!(
            matches!(kinds(&editor)[0], BlockKind::CodeBlock { .. }),
            "code is code, whatever chord is pressed in it"
        );
    }

    phase a_heading_rides_the_blocks_the_editor_writes_as_an_edit;
    {
        let mut doc = AutoCommit::new();
        let mut editor = editing(&["Plan"]);
        assert!(chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "2"
        ));
        let content = editor.blocks()[0].content();
        assert_eq!(content.kind, BlockKind::Heading(2));
        assert_eq!(content.text, "Plan");
    }

    phase a_heading_chosen_in_a_map_shows_the_block_it_names_each_time_it_is_chosen;
    {
        let mut blocks = vec![
            BlockContent::paragraph("b0", "Intro"),
            BlockContent::paragraph("b1", "Ship it"),
            BlockContent::paragraph("b2", "Then"),
            BlockContent::paragraph("b3", "Grow it"),
        ];
        blocks[1].kind = BlockKind::Heading(2);
        blocks[2].kind = BlockKind::Heading(3);
        blocks[3].kind = BlockKind::Heading(2);
        let mut editor = BlockEditor::new();
        editor.present_blocks(&blocks);
        editor.focus_block(0, CursorPlacement::End);

        // A map's row names the heading by its place among the document's headings: the third
        // heading stands at the document's fourth block, with a paragraph before the first.
        editor.reveal_asked(Some("2:0".into()));
        assert_eq!(
            editor.focused_index(),
            Some(3),
            "the editor shows the block the heading stands at"
        );
        // The person moves on, and chooses the same heading again: a new ask is answered.
        editor.focus_block(0, CursorPlacement::End);
        editor.reveal_asked(Some("2:0".into()));
        assert_eq!(
            editor.focused_index(),
            Some(0),
            "the ask already answered is not answered again"
        );
        editor.reveal_asked(Some("2:1".into()));
        assert_eq!(
            editor.focused_index(),
            Some(3),
            "the same heading asked again is shown again"
        );
        editor.reveal_asked(Some("9:2".into()));
        assert_eq!(
            editor.focused_index(),
            Some(3),
            "a heading the document does not hold shows nothing"
        );

        // What a map of the document reads: each heading's own words, and the level it stands
        // at, never the marker a writer typed.
        assert_eq!(
            editor.headings(),
            vec![
                (2, "Ship it".to_owned()),
                (3, "Then".to_owned()),
                (2, "Grow it".to_owned()),
            ]
        );
    }

    phase a_block_a_search_hit_names_by_its_place_is_shown_and_the_headings_ask_stays_answered;
    {
        let mut blocks = vec![
            BlockContent::paragraph("b0", "Intro"),
            BlockContent::paragraph("b1", "Plan"),
            BlockContent::paragraph("b2", "The budget is tight"),
            BlockContent::paragraph("b3", "Then"),
        ];
        blocks[1].kind = BlockKind::Heading(2);
        let mut editor = BlockEditor::new();
        editor.present_blocks(&blocks);
        // The person chose the note's first heading in its map.
        editor.reveal_asked(Some("0:1".into()));
        assert_eq!(editor.focused_index(), Some(1));

        // A search hit on the third paragraph opens the note: the window asks for that block by
        // its place, whatever kind it is.
        editor.reveal_asked(Some("¶2:1".into()));
        assert_eq!(
            editor.focused_index(),
            Some(2),
            "the paragraph the hit names is shown"
        );
        // The view still says the heading it asked for before: that is no new ask.
        editor.reveal_asked(Some("0:1".into()));
        assert_eq!(
            editor.focused_index(),
            Some(2),
            "the heading asked for before is not shown again"
        );
        // The same paragraph hit again is shown again; one past the end shows nothing.
        editor.focus_block(0, CursorPlacement::End);
        editor.reveal_asked(Some("¶2:2".into()));
        assert_eq!(editor.focused_index(), Some(2));
        editor.reveal_asked(Some("¶9:3".into()));
        assert_eq!(editor.focused_index(), Some(2));

        // A new editor shows the block it is asked for at once, and takes a heading asked of the
        // note before as answered.
        let mut opened = BlockEditor::new();
        opened.present_blocks(&blocks);
        opened.reveal_opened(Some("¶3:4".into()));
        assert_eq!(opened.focused_index(), Some(3));
        let mut another = BlockEditor::new();
        another.present_blocks(&blocks);
        another.reveal_opened(Some("0:1".into()));
        another.reveal_asked(Some("0:1".into()));
        assert_ne!(
            another.focused_index(),
            Some(1),
            "a heading asked of the note shown before is no place in this one"
        );
    }
}
