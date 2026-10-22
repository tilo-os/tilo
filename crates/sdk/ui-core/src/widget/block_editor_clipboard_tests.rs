//! Cut, copy and paste carry formatting: the editor's copy offers HTML
//! with the words, its paste reads what Writer, Notes, Mail, a browser or
//! an office suite copied, and Ctrl+Shift+V pastes the words alone.

use super::*;
use crate::testing::Dispatch;
use crate::widget::marks::{Mark, MarkSpan};
use automerge::ROOT;
use winit::keyboard::ModifiersState;

fn chord(editor: &mut BlockEditor, doc: &mut AutoCommit, modifiers: ModifiersState, key: &str) {
    let event =
        crate::synthetic_input::key_event(Key::Character(key.into()), None, ElementState::Pressed);
    let handled = Dispatch::new()
        .modifiers(modifiers)
        .doc(doc)
        .send(editor, &crate::synthetic_input::keyboard_input(event))
        .handled;
    assert!(handled, "the editor takes {modifiers:?}+{key}");
    editor.sync(doc, &ROOT);
}

fn para(text: &str, marks: Vec<MarkSpan>) -> BlockContent {
    let mut block = BlockContent::paragraph("", text);
    block.marks = marks;
    block
}

fn editing(blocks: &[BlockContent]) -> BlockEditor {
    let mut editor = BlockEditor::new();
    editor.present_blocks(blocks);
    editor.set_focus(true);
    editor
}

fn contents(editor: &BlockEditor) -> Vec<(BlockKind, String, Vec<MarkSpan>)> {
    editor
        .blocks()
        .iter()
        .map(|block| {
            let content = block.content();
            (content.kind, content.text, content.marks)
        })
        .collect()
}

fn at(block: usize, byte: usize) -> DocumentPosition {
    DocumentPosition { block, byte }
}

#[test]
fn copy_and_paste_carry_formatting() {
    fn ctrl_c_offers_the_selection_as_html_and_as_words() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        let mut editor = editing(&[
            BlockContent::paragraph("", "Plan").with_kind(BlockKind::Heading(1)),
            para("make it bold", vec![MarkSpan::new(8..12, Mark::Bold)]),
            BlockContent::paragraph("", "item").with_kind(BlockKind::Bullet { indent: 0 }),
        ]);
        editor.select_document_range(at(0, 0), at(2, 4));
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "c");
        let html = recorder.rich_copies();
        assert_eq!(html.len(), 1, "one copy: {html:?}");
        assert!(html[0].contains("<h1>Plan</h1>"), "{}", html[0]);
        assert!(html[0].contains("<strong>bold</strong>"), "{}", html[0]);
        assert!(html[0].contains("<li>item</li>"), "{}", html[0]);
        let words = recorder.copies();
        assert_eq!(
            words.last().map(|(_, text)| text.as_str()),
            Some("Plan\nmake it bold\nitem")
        );
        assert_eq!(editor.blocks().len(), 3, "a copy changes nothing");
    }

    fn ctrl_v_brings_in_headings_lists_and_marks() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        assert!(crate::clipboard_copy_rich_accepted(
            "<h2>Agenda</h2><p>Be <b>brief</b></p><ol><li>one</li></ol>",
            "Agenda\nBe brief\none",
        ));
        let mut editor = editing(&[BlockContent::paragraph("", "")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "v");
        assert_eq!(
            contents(&editor),
            [
                (BlockKind::Heading(2), "Agenda".into(), vec![]),
                (
                    BlockKind::Paragraph,
                    "Be brief".into(),
                    vec![MarkSpan::new(3..8, Mark::Bold)]
                ),
                (BlockKind::Numbered { indent: 0 }, "one".into(), vec![]),
            ]
        );
    }

    fn a_paste_inside_words_joins_them_at_both_ends() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        assert!(crate::clipboard_copy_rich_accepted("<i>very</i> ", "very "));
        let mut editor = editing(&[BlockContent::paragraph("", "a good day")]);
        editor.focus_block(0, CursorPlacement::Byte(2));
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "v");
        assert_eq!(
            contents(&editor),
            [(
                BlockKind::Paragraph,
                "a very good day".into(),
                vec![MarkSpan::new(2..6, Mark::Italic)]
            )]
        );
    }

    fn ctrl_shift_v_pastes_the_words_alone() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        assert!(crate::clipboard_copy_rich_accepted(
            "<h1><b>Loud</b></h1>",
            "Loud"
        ));
        let mut editor = editing(&[BlockContent::paragraph("", "")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        chord(
            &mut editor,
            &mut doc,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            "v",
        );
        assert_eq!(
            contents(&editor),
            [(BlockKind::Paragraph, "Loud".into(), vec![])]
        );
    }

    fn code_takes_the_words_not_the_formatting() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        assert!(crate::clipboard_copy_rich_accepted(
            "<h1>x = <b>1</b></h1>",
            "x = 1"
        ));
        let mut editor = editing(&[
            BlockContent::paragraph("", "").with_kind(BlockKind::CodeBlock { lang: None })
        ]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "v");
        assert_eq!(
            contents(&editor),
            [(BlockKind::CodeBlock { lang: None }, "x = 1".into(), vec![])]
        );
    }

    fn plain_words_still_paste_as_words() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        crate::clipboard_copy("<b>not html</b>");
        let mut editor = editing(&[BlockContent::paragraph("", "")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "v");
        assert_eq!(
            contents(&editor),
            [(BlockKind::Paragraph, "<b>not html</b>".into(), vec![])]
        );
    }

    fn a_cut_copies_formatting_then_removes_the_words() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        let mut editor = editing(&[para("keep cut", vec![MarkSpan::new(5..8, Mark::Italic)])]);
        editor.select_document_range(at(0, 4), at(0, 8));
        let mut doc = AutoCommit::new();
        chord(&mut editor, &mut doc, ModifiersState::CONTROL, "x");
        assert!(
            recorder
                .rich_copies()
                .last()
                .is_some_and(|html| html.contains("<em>cut</em>"))
        );
        assert_eq!(
            contents(&editor),
            [(BlockKind::Paragraph, "keep".into(), vec![])]
        );
    }

    fn the_copy_round_trips_into_another_editor() {
        let (services, _recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        let source = [
            BlockContent::paragraph("", "done").with_kind(BlockKind::Checklist {
                indent: 0,
                done: true,
            }),
            para(
                "see site",
                vec![MarkSpan::new(
                    4..8,
                    Mark::ExternalLink("https://tilo.so".into()),
                )],
            ),
            BlockContent::paragraph("", "said").with_kind(BlockKind::Quote),
        ];
        let mut writer = editing(&source);
        writer.select_document_range(at(0, 0), at(2, 4));
        let mut doc = AutoCommit::new();
        chord(&mut writer, &mut doc, ModifiersState::CONTROL, "c");
        let mut notes = editing(&[BlockContent::paragraph("", "")]);
        notes.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        chord(&mut notes, &mut doc, ModifiersState::CONTROL, "v");
        assert_eq!(contents(&notes), contents(&writer));
    }

    ctrl_c_offers_the_selection_as_html_and_as_words();
    ctrl_v_brings_in_headings_lists_and_marks();
    a_paste_inside_words_joins_them_at_both_ends();
    ctrl_shift_v_pastes_the_words_alone();
    code_takes_the_words_not_the_formatting();
    plain_words_still_paste_as_words();
    a_cut_copies_formatting_then_removes_the_words();
    the_copy_round_trips_into_another_editor();
}

#[test]
fn the_text_menu_is_accessible() {
    fn find<'a>(
        node: &'a crate::a11y::AccessNode,
        key: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
        if node.semantic_key.as_deref() == Some(key) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, key))
    }
    let (services, recorder) = crate::host::HostServices::recording();
    let _host = crate::host::enter(&services);
    let mut editor = editing(&[para("copy me", vec![MarkSpan::new(0..4, Mark::Bold)])]);
    editor.blocks[0]
        .line
        .apply_layout_external(Rect::new(0.0, 0.0, 300.0, 24.0));
    editor.select_document_range(at(0, 0), at(0, 7));
    assert!(editor.open_text_menu(Point::new(10.0, 12.0)));
    let tree = Widget::accessibility(&editor).expect("the editor is in the tree");
    let menu = find(&tree, "editor.text-menu").expect("the open menu is in the tree");
    assert_eq!(menu.role, crate::a11y::Role::Menu);
    let row = |name: &str| {
        menu.children
            .iter()
            .find(|node| node.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("the menu offers {name}: {:?}", menu.children))
    };
    let _ = row("Paste without formatting");
    let copy = row("Copy")
        .action_target
        .clone()
        .expect("a row is actionable");
    let mut doc = AutoCommit::new();
    assert!(editor.perform_semantic_action(
        &copy,
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut doc),
        &ROOT,
    ));
    assert!(
        recorder
            .rich_copies()
            .last()
            .is_some_and(|html| html.contains("<strong>copy</strong>")),
        "activating Copy copies, formatted: {:?}",
        recorder.rich_copies()
    );
    assert!(editor.text_menu().is_none(), "a chosen row closes the menu");
}

trait WithKind {
    fn with_kind(self, kind: BlockKind) -> Self;
}

impl WithKind for BlockContent {
    fn with_kind(mut self, kind: BlockKind) -> Self {
        self.kind = kind;
        self
    }
}
