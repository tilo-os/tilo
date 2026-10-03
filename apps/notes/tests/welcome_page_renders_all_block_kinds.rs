//! Pins the Welcome page seed across all four Phase-3 block kinds.
//!
//! Two assertions:
//! 1. The Welcome page body round-trips through the markdown parser
//!    byte-stably (so saving + reloading produces identical content).
//! 2. Loading the body into a `BlockEditor` produces blocks of every
//!    Phase-3 kind (paragraph, heading, bullet, checklist, code block,
//!    divider) — proving the parser landed at least one of each.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashSet;

use notes::WELCOME_BODY;
use notes::markdown::{BlockSpec, parse, serialize};

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
