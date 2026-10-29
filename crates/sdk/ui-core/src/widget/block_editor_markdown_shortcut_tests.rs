//! Markdown typed as structure and links: a rule, the deeper headings, `[text](url)` and a
//! typed web address.

use super::*;
use crate::testing::Dispatch;
use automerge::ROOT;

fn send(editor: &mut BlockEditor, doc: &mut AutoCommit, key: Key) {
    let event = crate::synthetic_input::key_event(
        key.clone(),
        match &key {
            Key::Character(text) => Some(text.clone()),
            _ => None,
        },
        ElementState::Pressed,
    );
    Dispatch::new()
        .doc(doc)
        .send(editor, &crate::synthetic_input::keyboard_input(event));
    editor.sync(doc, &ROOT);
}

fn type_text(editor: &mut BlockEditor, doc: &mut AutoCommit, text: &str) {
    for character in text.chars() {
        send(editor, doc, Key::Character(character.to_string().into()));
    }
    editor.settle_typing();
}

fn typed(text: &str) -> BlockEditor {
    let mut doc = AutoCommit::new();
    let mut editor = BlockEditor::new();
    editor.focus_block(0, CursorPlacement::Start);
    type_text(&mut editor, &mut doc, text);
    editor
}

#[test]
fn a_typed_rule_is_a_divider_and_typing_goes_on_below_it() {
    let mut doc = AutoCommit::new();
    let mut editor = BlockEditor::new();
    editor.focus_block(0, CursorPlacement::Start);
    type_text(&mut editor, &mut doc, "---");
    assert_eq!(editor.blocks()[0].kind, BlockKind::Divider);
    type_text(&mut editor, &mut doc, "Next");
    assert_eq!(editor.blocks().len(), 2);
    assert_eq!(editor.blocks()[1].text(), "Next");
    // In prose, three dashes are three dashes.
    let prose = typed("a---");
    assert_eq!(prose.blocks()[0].kind, BlockKind::Paragraph);
    assert_eq!(prose.blocks()[0].text(), "a---");
}

#[test]
fn four_to_six_hashes_make_the_deeper_headings_and_keep_their_level() {
    for (prefix, level) in [("#### ", 4), ("##### ", 5), ("###### ", 6)] {
        let editor = typed(&format!("{prefix}Notes"));
        assert_eq!(
            editor.blocks()[0].kind,
            BlockKind::Heading(level),
            "{prefix:?}"
        );
        assert_eq!(editor.blocks()[0].text(), "Notes");
        assert_eq!(editor.to_markdown().trim(), format!("{prefix}Notes"));
    }
    let seven = typed("####### Notes");
    assert_eq!(
        seven.blocks()[0].kind,
        BlockKind::Paragraph,
        "Markdown has six"
    );
}

#[test]
fn a_typed_link_and_a_typed_address_become_links() {
    let editor = typed("see [the plan](https://tilo.so/plan)");
    assert_eq!(editor.blocks()[0].text(), "see the plan");
    assert_eq!(
        editor.blocks()[0].line.marks(),
        [MarkSpan::new(
            4..12,
            Mark::ExternalLink("https://tilo.so/plan".into())
        )]
    );

    let resource = typed("[Standup](tilo://open#/me/so.tilo.calendar/-/calendar.event:e-1)");
    assert_eq!(resource.blocks()[0].text(), "Standup");
    assert_eq!(
        resource.blocks()[0].line.marks(),
        [MarkSpan::new(
            0..7,
            Mark::ResourceLink("tilo://open#/me/so.tilo.calendar/-/calendar.event:e-1".into())
        )]
    );

    let address = typed("read https://tilo.so/docs. then");
    assert_eq!(
        address.blocks()[0].text(),
        "read https://tilo.so/docs. then",
        "the address stays as typed"
    );
    assert_eq!(
        address.blocks()[0].line.marks(),
        [MarkSpan::new(
            5..25,
            Mark::ExternalLink("https://tilo.so/docs".into())
        )]
    );
}
