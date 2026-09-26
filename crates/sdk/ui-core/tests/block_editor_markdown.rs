//! Round-trip tests for `BlockEditor::from_markdown` /
//! `BlockEditor::to_markdown`.
//!
//! The parser is line-based, with no inline marks. The serializer joins
//! blocks with a single blank line and always emits a trailing newline.
//! These two are inverses for the subset they cover — any document the
//! editor can produce will re-parse byte-stably back into the same block
//! list.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::{BlockEditor, BlockKind};

/// One expected block: its kind and text, each `None` where the case does
/// not pin it.
type Want = (Option<BlockKind>, Option<&'static str>);

fn kind(k: BlockKind) -> Want {
    (Some(k), None)
}

fn assert_blocks(case: &str, editor: &BlockEditor, want: &[Want]) {
    assert_eq!(
        editor.blocks().len(),
        want.len(),
        "case {case}: block count"
    );
    for (i, ((want_kind, want_text), block)) in want.iter().zip(editor.blocks()).enumerate() {
        if let Some(k) = want_kind {
            assert_eq!(&block.kind, k, "case {case}: kind of block {i}");
        }
        if let Some(t) = want_text {
            assert_eq!(block.text(), *t, "case {case}: text of block {i}");
        }
    }
}

/// Every document the editor can produce parses into the expected blocks and
/// serialises back byte-stably.
#[allow(
    clippy::too_many_lines,
    reason = "the case table lists every block kind the grammar covers"
)]
#[test]
fn markdown_round_trips_every_block_kind_byte_stably() {
    use BlockKind::{Bullet, Checklist, CodeBlock, Divider, Heading, Paragraph};
    // The full corpus the Welcome page will ship with — every kind
    // present exactly once. If this round-trips byte-stably the
    // markdown grammar handles realistic documents.
    let kitchen_sink = "\
# Welcome

This is a paragraph.

## Subtitle

- bullet one

  - nested

- [ ] todo

- [x] done

```rust
fn main() {}
```

---

after the rule
";
    let cases: Vec<(&str, &str, Option<Vec<Want>>)> = vec![
        (
            "heading_plus_two_paragraphs",
            "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n",
            Some(vec![
                (Some(Heading(1)), Some("Title")),
                (Some(Paragraph), Some("First paragraph.")),
                (Some(Paragraph), Some("Second paragraph.")),
            ]),
        ),
        (
            "only_heading",
            "## Subtitle\n",
            Some(vec![(Some(Heading(2)), Some("Subtitle"))]),
        ),
        (
            "multiple_heading_levels",
            "# H1\n\n## H2\n\n### H3\n\nbody\n",
            Some(vec![
                kind(Heading(1)),
                kind(Heading(2)),
                kind(Heading(3)),
                kind(Paragraph),
            ]),
        ),
        (
            "two_paragraphs_no_heading",
            "First.\n\nSecond.\n",
            Some(vec![(None, None), (None, None)]),
        ),
        (
            "bullet_list_with_indent",
            "- a\n\n  - b\n\n    - c\n",
            Some(vec![
                kind(Bullet { indent: 0 }),
                kind(Bullet { indent: 1 }),
                kind(Bullet { indent: 2 }),
            ]),
        ),
        (
            "checklist_done_and_undone",
            "- [ ] todo\n\n- [x] done\n",
            Some(vec![
                kind(Checklist {
                    indent: 0,
                    done: false,
                }),
                kind(Checklist {
                    indent: 0,
                    done: true,
                }),
            ]),
        ),
        (
            "code_block_with_lang",
            "```rust\nfn main() {}\n```\n",
            Some(vec![(
                Some(CodeBlock {
                    lang: Some("rust".into()),
                }),
                Some("fn main() {}"),
            )]),
        ),
        (
            "code_block_no_lang",
            "```\nplain code\n```\n",
            Some(vec![kind(CodeBlock { lang: None })]),
        ),
        (
            "divider",
            "before\n\n---\n\nafter\n",
            Some(vec![(None, None), kind(Divider), (None, None)]),
        ),
        ("mixed_kitchen_sink", kitchen_sink, None),
    ];
    for (case, md, want) in cases {
        let editor = BlockEditor::from_markdown(md);
        if let Some(want) = want {
            assert_blocks(case, &editor, &want);
        }
        assert_eq!(editor.to_markdown(), md, "case {case}: serialisation");
    }
}

/// Inputs that are not in the serialiser's canonical form parse to the
/// blocks a user can recover, and serialise to the canonical form.
#[test]
fn markdown_parser_normalises_blank_lines_and_bare_markers() {
    let cases: [(&str, &str, Want, Option<&str>); 4] = [
        (
            "empty_document_yields_one_empty_paragraph",
            "",
            (Some(BlockKind::Paragraph), Some("")),
            Some("\n"),
        ),
        // The parser collapses blank lines — they're separators, not empty blocks.
        (
            "document_with_only_blank_lines_yields_one_empty_paragraph",
            "\n\n\n",
            (None, Some("")),
            None,
        ),
        // A document ending in "...body.\n\n" (a paragraph then a stray
        // empty line) round-trips back to "...body.\n", since the parser
        // doesn't create a phantom empty block at EOF.
        (
            "trailing_blank_lines_are_ignored",
            "body.\n\n",
            (None, Some("body.")),
            Some("body.\n"),
        ),
        // A bare "#" without a trailing space + content is NOT a heading
        // per the markdown shortcut rule. The parser keeps it as a
        // paragraph so the user can recover it.
        (
            "heading_with_no_text_is_paragraph",
            "#\n",
            (Some(BlockKind::Paragraph), Some("#")),
            None,
        ),
    ];
    for (case, md, want, serialised) in cases {
        let editor = BlockEditor::from_markdown(md);
        assert_blocks(case, &editor, &[want]);
        if let Some(out) = serialised {
            assert_eq!(editor.to_markdown(), out, "case {case}: serialisation");
        }
    }
}
