//! Headings made from the format menu and its chords: writing that is already there takes the line's
//! kind, its text and marks stay, and body text is the way back.

use super::*;
use crate::testing::Dispatch;
use automerge::ROOT;
use winit::keyboard::{Key, ModifiersState};

fn chord(
    editor: &mut BlockEditor,
    doc: &mut AutoCommit,
    modifiers: ModifiersState,
    key: &str,
) -> bool {
    let event =
        crate::synthetic_input::key_event(Key::Character(key.into()), None, ElementState::Pressed);
    let handled = Dispatch::new()
        .modifiers(modifiers)
        .doc(doc)
        .send(editor, &crate::synthetic_input::keyboard_input(event))
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
        node: &'a crate::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
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
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(doc),
            &ROOT,
        ),
        "the row {name} runs"
    );
    editor.sync(doc, &ROOT);
}

#[test]
fn the_format_menu_makes_a_line_a_heading_and_body_text_again() {
    let (services, _recorder) = crate::host::HostServices::recording();
    let _host = crate::host::enter(&services);
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

#[test]
fn a_heading_is_drawn_in_the_toolkit_heading_style() {
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

#[test]
fn the_chords_make_the_line_a_heading_of_their_level_and_body_text_again() {
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

#[test]
fn the_chord_makes_every_selected_paragraph_a_heading() {
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

#[test]
fn a_code_block_keeps_its_kind_when_the_chord_is_pressed() {
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

#[test]
fn a_heading_rides_the_blocks_the_editor_writes_as_an_edit() {
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
