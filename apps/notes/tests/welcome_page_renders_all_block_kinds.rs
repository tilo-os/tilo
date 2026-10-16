//! A note using every block kind:
//! 1. round-trips through the markdown parser byte-stably, and
//! 2. loads into a `BlockEditor` as blocks of every kind (paragraph,
//!    heading, bullet, checklist, code block, divider).

use std::collections::HashSet;

use notes::markdown::{BlockSpec, parse, serialize};

/// A note that uses each block kind at least once, as a quick reference
/// for the editing syntax would.
pub const WELCOME_BODY: &str = "\
# Welcome

This is your first note. Edit it, or add a note from the left.

Type here. **Bold**, *italic*, `code`, and [[Welcome]] links work inline.

## Formatting

Start a line with `#` for a heading, or `- ` for a bullet:

- Coffee

- Tea

- Hot chocolate

  - With marshmallows

Add `[ ]` after the dash to track to-dos:

- [ ] Something to finish

- [x] Something already done

Triple backticks make a code block:

```rust
fn hello() {}
```

Start with `> ` to call out a quotation:

> Notes should feel calm, fast, and connected.

Or number them when order matters:

1. First, add a note.

2. Then write something.

3. Finally, link it with `[[`.

Three dashes make a divider:

---

## Shortcuts

- `Cmd-K` jumps to any note by name

- `Cmd-N` adds a note under the selection

- `/` opens the block menu; `[[` links a note

- `?` opens the full shortcuts list

Double-click a title in the sidebar to rename it.
";

#[test]
fn welcome_body_contains_every_block_kind_and_round_trips() {
    eprintln!("phase welcome_body_round_trips_byte_stable");
    {
        let parsed = parse(WELCOME_BODY);
        let serialized = serialize(&parsed);
        assert_eq!(
            serialized, WELCOME_BODY,
            "Welcome page must round-trip byte-stably (saving + reloading is\nthe basic correctness contract for the seed)"
        );
    }

    eprintln!("phase welcome_body_contains_every_block_kind");
    {
        let parsed = parse(WELCOME_BODY);
        let mut seen: HashSet<&'static str> = HashSet::new();
        for b in &parsed {
            let tag = match b {
                BlockSpec::Paragraph(_) => "paragraph",
                BlockSpec::Quote(_) => "quote",
                BlockSpec::Heading { .. } => "heading",
                BlockSpec::Bullet { .. } => "bullet",
                BlockSpec::Numbered { .. } => "numbered",
                BlockSpec::Checklist { .. } => "checklist",
                BlockSpec::CodeBlock { .. } => "code",
                BlockSpec::Table { .. } => "table",
                BlockSpec::Divider => "divider",
                BlockSpec::Nested { .. } => "nested",
            };
            seen.insert(tag);
        }
        for kind in [
            "paragraph",
            "quote",
            "heading",
            "bullet",
            "numbered",
            "checklist",
            "code",
            "divider",
        ] {
            assert!(
                seen.contains(kind),
                "welcome page seed must include a {kind} block (got kinds: {seen:?})",
            );
        }
    }

    eprintln!("phase welcome_body_includes_both_done_and_undone_checklists");
    {
        let parsed = parse(WELCOME_BODY);
        let mut found_done = false;
        let mut found_undone = false;
        for b in &parsed {
            if let BlockSpec::Checklist { done, .. } = b {
                if *done {
                    found_done = true;
                } else {
                    found_undone = true;
                }
            }
        }
        assert!(found_done, "welcome page should demo a done checklist");
        assert!(found_undone, "welcome page should demo an undone checklist");
    }
}
