//! Byte-stable `parse → serialize` across every supported block kind
//! (paragraph, heading 1-3,
//! bullets with three indent levels, checklists done/not, fenced code
//! block, divider). Mirror of `crates/sdk/ui-core/tests/block_editor_markdown.rs` so we
//! catch any drift between the storage grammar and the widget grammar.
//!
//! The two consumers are intentionally separate (`apps/notes` should
//! not depend on `ui` for parsing) — this test ensures they emit the
//! same bytes for the same block list.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::markdown::{BlockSpec, parse, serialize};

#[test]
fn every_block_kind_round_trips_byte_stably() {
    // (case, markdown that must survive parse -> serialize unchanged)
    let cases = [
        ("paragraph only", "Hello, world.\n"),
        ("all heading levels", "# H1\n\n## H2\n\n### H3\n"),
        ("checklist done and not", "- [ ] todo\n\n- [x] done\n"),
        ("code block with rust lang", "```rust\nfn main() {}\n```\n"),
        ("code block without lang", "```\nplain\n```\n"),
        ("divider between paragraphs", "before\n\n---\n\nafter\n"),
        // Pins every block kind in one document. If this regresses, the
        // grammar has drifted — likely in the Phase-3 serializer.
        (
            "kitchen sink",
            "\
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
",
        ),
    ];
    for (case, md) in cases {
        assert_eq!(serialize(&parse(md)), md, "case {case}");
    }

    // Both `- ` and `* ` are accepted; we always emit `- ` so the
    // output is canonical.
    assert_eq!(serialize(&parse("* alpha\n")), "- alpha\n");
}

#[test]
fn bullet_indents_parse_up_to_the_cap_and_round_trip() {
    // (markdown, expected (indent, text) per bullet). Exactly 5 levels
    // (= 10 spaces) is the MAX_LIST_INDENT cap and should parse as a
    // bullet. The parser stops chewing indent groups at 5 — beyond that,
    // the line falls through to a paragraph (covered separately).
    let cases: [(&str, &[(u8, &str)]); 2] = [
        ("- a\n\n  - b\n\n    - c\n", &[(0, "a"), (1, "b"), (2, "c")]),
        ("          - deep\n", &[(5, "deep")]),
    ];
    for (md, want) in cases {
        let blocks = parse(md);
        let got: Vec<(u8, &str)> = blocks
            .iter()
            .map(|block| match block {
                BlockSpec::Bullet { indent, text } => (*indent, text.text.as_str()),
                b => panic!("expected Bullet in {md:?}, got {b:?}"),
            })
            .collect();
        assert_eq!(got, want, "bullets of {md:?}");
        assert_eq!(serialize(&blocks), md, "round-trip of {md:?}");
    }
}
