//! Byte-stable `parse → serialize` for documents that carry inline marks
//! (bold / italic / code / page link). Mirror of
//! `markdown_phase3_roundtrip.rs` but pinned on the inline-marker
//! grammar instead of the block-kind grammar.
//!
//! Corpus: bold in paragraph, italic in heading, code in bullet,
//! nested bold-italic, link in paragraph, kitchen sink.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::markdown::{parse, serialize};

#[test]
fn inline_marks_round_trip_byte_stably_in_every_block_kind() {
    // (case, markdown that must survive parse -> serialize unchanged)
    let cases = [
        ("bold in paragraph", "Hello **world** today.\n"),
        ("italic in heading", "# A *taste* of\n"),
        ("code in bullet", "- run `cargo build` first\n"),
        ("link in paragraph", "See [[Welcome]] for details.\n"),
        ("nested bold italic", "**bold *italic***\n"),
        (
            "multiple marks in one paragraph",
            "This has **bold**, *italic*, and `code`.\n",
        ),
        (
            "kitchen sink",
            "\
# Welcome **dear** friend

This paragraph has *italic* and **bold** and `code`.

- bullet with [[Welcome]] link

- [ ] todo with **bold**

Last paragraph.
",
        ),
        ("checklist with inline code", "- [x] run `make` daily\n"),
        ("heading 3 with link", "### See [[Tips]] for more\n"),
    ];
    for (case, md) in cases {
        assert_eq!(
            serialize(&parse(md)),
            md,
            "round-trip failed for case {case}: {md:?}"
        );
    }

    // Empty input parses to one empty paragraph and serializes to
    // just "\n". `parse(serialize(parse("")))` must equal "\n".
    assert_eq!(serialize(&parse("")), "\n");
}
