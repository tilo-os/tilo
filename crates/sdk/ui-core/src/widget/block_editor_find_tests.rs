use super::super::find_bar::{FindOptions, Findable};
use super::*;

#[test]
fn find_matches_unicode_safely_and_never_edits_or_moves_the_caret() {
    assert_eq!(
        case_folded_match_ranges("İstanbul CAFÉ café", "i\u{307}"),
        vec![(0, "İ".len())]
    );
    assert_eq!(
        case_folded_match_ranges("İstanbul CAFÉ café", "CAFÉ"),
        vec![(10, 15), (16, 21)]
    );

    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("alpha beta alpha"),
        Block::paragraph("ALPHA"),
    ]);
    let before = editor.to_markdown();
    editor.focus_block(0, CursorPlacement::Byte(5));
    let found = editor.find("alpha", FindOptions::default());
    assert_eq!(
        found.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
        vec![(0, 5), (11, 16), (17, 22)],
        "one span per match, in the document's reading order"
    );
    editor.reveal(2);
    assert_eq!(editor.find_active, Some(2));
    assert_eq!(editor.focused_index(), Some(0), "the caret stays");
    assert_eq!(editor.blocks()[0].line.cursor_pos(), 5);
    assert_eq!(editor.to_markdown(), before);
    assert!(editor.take_edit_ops().is_empty());
}

fn findable_editor() -> BlockEditor {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Ä😀 alpha ALPHA"),
        Block::paragraph("日本 alpha"),
    ]);
    editor
}

#[test]
fn the_window_search_replaces_only_where_a_host_applies_it() {
    let mut editor = findable_editor();
    editor.find("alpha", FindOptions::default());
    assert!(!editor.supports_replace(), "no host applies a replacement");
    assert!(!editor.replace(0, "beta"));
    assert_eq!(editor.replace_all("beta"), 0);
    assert!(editor.take_find_replace().is_none());

    editor.set_find_replace_enabled(true);
    editor.set_read_only(true);
    assert!(!editor.supports_replace(), "a read-only document");
    assert!(!editor.replace(0, "beta"));
}

#[test]
fn replacing_one_match_hands_the_host_the_match_in_its_block() {
    let mut editor = findable_editor();
    editor.set_find_replace_enabled(true);
    editor.find("alpha", FindOptions::default());
    assert!(editor.supports_replace());
    let before = editor.to_markdown();
    assert!(editor.replace(2, "β😀"));
    assert!(
        !editor.replace(0, "β"),
        "one replacement waits for its host at a time"
    );
    let Some(FindReplace::One(target, with)) = editor.take_find_replace() else {
        panic!("one match to replace");
    };
    assert_eq!(target.block.id, editor.blocks()[1].id);
    assert_eq!(
        target.block.text.get(target.start_byte..target.end_byte),
        Some("alpha")
    );
    assert_eq!(with, "β😀");
    assert_eq!(editor.to_markdown(), before, "the host writes it");
    assert!(editor.replace(0, "β"), "the next one is taken");
    assert!(
        !editor.replace(0, "a\nb"),
        "no line breaks in a replacement"
    );
}

#[test]
fn a_match_found_in_text_that_since_changed_is_not_replaced() {
    let mut editor = findable_editor();
    editor.set_find_replace_enabled(true);
    editor.find("alpha", FindOptions::default());
    editor.set_blocks(vec![
        Block::paragraph("Peer Ä😀 alpha ALPHA"),
        Block::paragraph("日本 alpha"),
    ]);
    assert!(!editor.replace(0, "β"));
    assert_eq!(editor.replace_all("β"), 0);
    assert!(editor.take_find_replace().is_none());
    editor.find("alpha", FindOptions::default());
    assert!(editor.replace(0, "β"), "found again, the match is current");
}

#[test]
fn replacing_every_match_hands_the_host_one_snapshot_of_the_blocks() {
    let mut editor = findable_editor();
    editor.set_find_replace_enabled(true);
    editor.find("alpha", FindOptions::default());
    assert_eq!(editor.replace_all("β"), 3);
    let Some(FindReplace::All(batch)) = editor.take_find_replace() else {
        panic!("every match to replace");
    };
    assert_eq!(batch.blocks.len(), 2);
    assert_eq!(batch.replacement, "β");
    let matched: Vec<&str> = batch
        .matches
        .iter()
        .filter_map(|(block, range)| batch.blocks.get(*block)?.text.get(range.clone()))
        .collect();
    assert_eq!(matched, ["alpha", "ALPHA", "alpha"]);

    editor.find("absent", FindOptions::default());
    assert_eq!(editor.replace_all("β"), 0, "nothing to replace");
    assert!(editor.take_find_replace().is_none());
}
