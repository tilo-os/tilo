//! Round-trip tests for `BlockEditor::from_markdown` /
//! `BlockEditor::to_markdown`.
//!
//! The parser is line-based (no inline marks, no fenced code, no
//! lists in Phase 2). The serializer joins blocks with a single
//! blank line and always emits a trailing newline. These two are
//! inverses for the subset they cover — any document the editor can
//! produce will re-parse byte-stably back into the same block list.

use ui::widget::{BlockEditor, BlockKind};

#[test]
fn round_trip_heading_plus_two_paragraphs() {
    let md = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n";
    let editor = BlockEditor::from_markdown(md);
    assert_eq!(editor.blocks().len(), 3);
    assert!(matches!(editor.blocks()[0].kind, BlockKind::Heading(1)));
    assert_eq!(editor.blocks()[0].text(), "Title");
    assert!(matches!(editor.blocks()[1].kind, BlockKind::Paragraph));
    assert_eq!(editor.blocks()[1].text(), "First paragraph.");
    assert!(matches!(editor.blocks()[2].kind, BlockKind::Paragraph));
    assert_eq!(editor.blocks()[2].text(), "Second paragraph.");
    assert_eq!(editor.to_markdown(), md);
}

#[test]
fn round_trip_only_heading() {
    let md = "## Subtitle\n";
    let editor = BlockEditor::from_markdown(md);
    assert_eq!(editor.blocks().len(), 1);
    assert!(matches!(editor.blocks()[0].kind, BlockKind::Heading(2)));
    assert_eq!(editor.blocks()[0].text(), "Subtitle");
    assert_eq!(editor.to_markdown(), md);
}

#[test]
fn round_trip_multiple_heading_levels() {
    let md = "# H1\n\n## H2\n\n### H3\n\nbody\n";
    let editor = BlockEditor::from_markdown(md);
    assert_eq!(editor.blocks().len(), 4);
    assert!(matches!(editor.blocks()[0].kind, BlockKind::Heading(1)));
    assert!(matches!(editor.blocks()[1].kind, BlockKind::Heading(2)));
    assert!(matches!(editor.blocks()[2].kind, BlockKind::Heading(3)));
    assert!(matches!(editor.blocks()[3].kind, BlockKind::Paragraph));
    assert_eq!(editor.to_markdown(), md);
}

#[test]
fn empty_document_yields_one_empty_paragraph() {
    let editor = BlockEditor::from_markdown("");
    assert_eq!(editor.blocks().len(), 1);
    assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    assert_eq!(editor.blocks()[0].text(), "");
    // Re-serialised: a single empty paragraph + trailing newline.
    assert_eq!(editor.to_markdown(), "\n");
}

#[test]
fn document_with_only_blank_lines_yields_one_empty_paragraph() {
    // The parser collapses blank lines — they're separators, not
    // empty blocks.
    let editor = BlockEditor::from_markdown("\n\n\n");
    assert_eq!(editor.blocks().len(), 1);
    assert_eq!(editor.blocks()[0].text(), "");
}

#[test]
fn trailing_blank_lines_are_ignored() {
    // A document ending in "...body.\n\n" (a paragraph then a stray
    // empty line) round-trips back to "...body.\n", since the parser
    // doesn't create a phantom empty block at EOF.
    let editor = BlockEditor::from_markdown("body.\n\n");
    assert_eq!(editor.blocks().len(), 1);
    assert_eq!(editor.blocks()[0].text(), "body.");
    assert_eq!(editor.to_markdown(), "body.\n");
}

#[test]
fn round_trip_two_paragraphs_no_heading() {
    let md = "First.\n\nSecond.\n";
    let editor = BlockEditor::from_markdown(md);
    assert_eq!(editor.blocks().len(), 2);
    assert_eq!(editor.to_markdown(), md);
}

#[test]
fn heading_with_no_text_is_paragraph() {
    // A bare "#" without a trailing space + content is NOT a heading
    // per the markdown shortcut rule. The parser keeps it as a
    // paragraph so the user can recover it.
    let editor = BlockEditor::from_markdown("#\n");
    assert_eq!(editor.blocks().len(), 1);
    assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    assert_eq!(editor.blocks()[0].text(), "#");
}
