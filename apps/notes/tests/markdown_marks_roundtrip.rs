//! Phase 4: byte-stable `parse → serialize` for documents that carry
//! inline marks (bold / italic / code / page link). Mirror of
//! `markdown_phase3_roundtrip.rs` but pinned on the inline-marker
//! grammar instead of the block-kind grammar.
//!
//! Corpus: bold in paragraph, italic in heading, code in bullet,
//! nested bold-italic, link in paragraph, kitchen sink.

use notes::markdown::{parse, serialize};

fn round_trip(md: &str) {
    let back = serialize(&parse(md));
    assert_eq!(back, md, "round-trip failed for {:?}", md);
}

#[test]
fn bold_in_paragraph() {
    round_trip("Hello **world** today.\n");
}

#[test]
fn italic_in_heading() {
    round_trip("# A *taste* of\n");
}

#[test]
fn code_in_bullet() {
    round_trip("- run `cargo build` first\n");
}

#[test]
fn link_in_paragraph() {
    round_trip("See [[Welcome]] for details.\n");
}

#[test]
fn nested_bold_italic() {
    round_trip("**bold *italic***\n");
}

#[test]
fn multiple_marks_same_paragraph() {
    round_trip("This has **bold**, *italic*, and `code`.\n");
}

#[test]
fn kitchen_sink() {
    let md = "\
# Welcome **dear** friend

This paragraph has *italic* and **bold** and `code`.

- bullet with [[Welcome]] link

- [ ] todo with **bold**

Last paragraph.
";
    round_trip(md);
}

#[test]
fn checklist_with_inline_code() {
    round_trip("- [x] run `make` daily\n");
}

#[test]
fn heading3_with_link() {
    round_trip("### See [[Tips]] for more\n");
}

#[test]
fn empty_input_round_trips_to_empty_paragraph_newline() {
    // Empty input parses to one empty paragraph and serializes to
    // just "\n". `parse(serialize(parse("")))` must equal "\n".
    assert_eq!(serialize(&parse("")), "\n");
}
