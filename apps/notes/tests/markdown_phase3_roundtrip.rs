//! Phase 3 round-trip pinning: byte-stable `parse → serialize` across
//! every block kind the notes app now supports (paragraph, heading 1-3,
//! bullets with three indent levels, checklists done/not, fenced code
//! block, divider). Mirror of `ui/tests/block_editor_markdown.rs` so we
//! catch any drift between the storage grammar and the widget grammar.
//!
//! The two consumers are intentionally separate (`apps/notes` should
//! not depend on `ui` for parsing) — this test ensures they emit the
//! same bytes for the same block list.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use notes::markdown::{BlockSpec, parse, serialize};

#[test]
fn round_trip_paragraph_only() {
    let md = "Hello, world.\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_all_heading_levels() {
    let md = "# H1\n\n## H2\n\n### H3\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_bullets_at_three_indents() {
    let md = "- a\n\n  - b\n\n    - c\n";
    let blocks = parse(md);
    assert_eq!(blocks.len(), 3);
    match &blocks[0] {
        BlockSpec::Bullet { indent, text } => {
            assert_eq!(*indent, 0);
            assert_eq!(text.text, "a");
        }
        b => panic!("expected Bullet, got {b:?}"),
    }
    match &blocks[1] {
        BlockSpec::Bullet { indent, .. } => assert_eq!(*indent, 1),
        b => panic!("expected Bullet, got {b:?}"),
    }
    match &blocks[2] {
        BlockSpec::Bullet { indent, .. } => assert_eq!(*indent, 2),
        b => panic!("expected Bullet, got {b:?}"),
    }
    assert_eq!(serialize(&blocks), md);
}

#[test]
fn round_trip_checklist_done_and_not() {
    let md = "- [ ] todo\n\n- [x] done\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_code_block_rust_lang() {
    let md = "```rust\nfn main() {}\n```\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_code_block_no_lang() {
    let md = "```\nplain\n```\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_divider_between_paragraphs() {
    let md = "before\n\n---\n\nafter\n";
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn round_trip_kitchen_sink() {
    // Pins every block kind in one document. If this regresses, the
    // grammar has drifted — likely in the Phase-3 serializer.
    let md = "\
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
    assert_eq!(serialize(&parse(md)), md);
}

#[test]
fn parse_accepts_max_indent_level() {
    // Exactly 5 levels (= 10 spaces) is the MAX_LIST_INDENT cap and
    // should parse as a bullet. The parser stops chewing indent
    // groups at 5 — beyond that, the line falls through to a
    // paragraph (covered separately).
    let md = "          - deep\n"; // 10 spaces + "- deep"
    let blocks = parse(md);
    match &blocks[0] {
        BlockSpec::Bullet { indent, text } => {
            assert_eq!(*indent, 5);
            assert_eq!(text.text, "deep");
        }
        b => panic!("expected Bullet, got {b:?}"),
    }
    assert_eq!(serialize(&blocks), md);
}

#[test]
fn star_bullet_normalises_to_dash() {
    // Both `- ` and `* ` are accepted; we always emit `- ` so the
    // output is canonical.
    let blocks = parse("* alpha\n");
    assert_eq!(serialize(&blocks), "- alpha\n");
}
