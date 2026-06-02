//! Pure functions to translate between a page's markdown body on
//! disk and the in-memory block list the [`ui::widget::BlockEditor`]
//! renders.
//!
//! Phase 2 keeps this dumb — line-based, no inline marks, no fenced
//! code, no lists. The grammar is:
//!
//! - A line beginning with `# `, `## `, `### ` is a heading.
//! - Any other non-empty line is a paragraph.
//! - Blank lines are block separators (consecutive blanks collapse).
//!
//! The serializer is the inverse: each block is one line, blocks are
//! joined by a single blank line, and the output ends in a trailing
//! newline. These rules are byte-stable on round-trip for everything
//! the editor can produce. See `ui/tests/block_editor_markdown.rs`
//! for the corpus of pinned cases.
//!
//! We intentionally re-derive the same grammar inside
//! [`ui::widget::BlockEditor::from_markdown`] / `to_markdown` so the
//! UI crate has no notes-specific knowledge — the duplication is the
//! seam, not the bug. This module is the right shape for the
//! markdown-only persistence loop the notes app uses; if a non-UI
//! consumer ever needs the block list, they reach for this module,
//! not the widget.

/// Storage-friendly block representation. Carries the kind tag + the
/// raw text content, with no UI / focus / cursor / RichTextLine
/// state. Parallel to `ui::widget::Block` but trivially clonable +
/// debuggable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSpec {
    Paragraph(String),
    /// 1, 2, or 3. Clamped on construction.
    Heading { level: u8, text: String },
}

impl BlockSpec {
    pub fn paragraph(s: impl Into<String>) -> Self {
        BlockSpec::Paragraph(s.into())
    }
    pub fn heading(level: u8, s: impl Into<String>) -> Self {
        BlockSpec::Heading {
            level: level.clamp(1, 3),
            text: s.into(),
        }
    }
}

/// Parse a markdown string into a flat list of [`BlockSpec`]s.
///
/// Empty input → a single empty paragraph (so callers always have
/// somewhere to start typing).
pub fn parse(md: &str) -> Vec<BlockSpec> {
    let mut blocks = Vec::new();
    for line in md.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            blocks.push(BlockSpec::heading(3, rest));
        } else if let Some(rest) = line.strip_prefix("## ") {
            blocks.push(BlockSpec::heading(2, rest));
        } else if let Some(rest) = line.strip_prefix("# ") {
            blocks.push(BlockSpec::heading(1, rest));
        } else {
            blocks.push(BlockSpec::paragraph(line));
        }
    }
    if blocks.is_empty() {
        blocks.push(BlockSpec::paragraph(""));
    }
    blocks
}

/// Inverse of [`parse`]. Always emits a trailing newline. Round-
/// trips byte-stably for any block list this module can produce.
pub fn serialize(blocks: &[BlockSpec]) -> String {
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        match b {
            BlockSpec::Paragraph(t) => out.push_str(t),
            BlockSpec::Heading { level, text } => {
                let prefix = match *level {
                    1 => "# ",
                    2 => "## ",
                    _ => "### ",
                };
                out.push_str(prefix);
                out.push_str(text);
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_yields_single_empty_paragraph() {
        let blocks = parse("");
        assert_eq!(blocks, vec![BlockSpec::paragraph("")]);
    }

    #[test]
    fn parse_heading_levels() {
        let blocks = parse("# H1\n\n## H2\n\n### H3\n");
        assert_eq!(
            blocks,
            vec![
                BlockSpec::heading(1, "H1"),
                BlockSpec::heading(2, "H2"),
                BlockSpec::heading(3, "H3"),
            ]
        );
    }

    #[test]
    fn parse_paragraph_with_punctuation() {
        let blocks = parse("Hello, world.\n");
        assert_eq!(blocks, vec![BlockSpec::paragraph("Hello, world.")]);
    }

    #[test]
    fn round_trip_typical_document() {
        let md = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_paragraphs_only() {
        let md = "alpha\n\nbeta\n\ngamma\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn lone_hash_is_paragraph_not_heading() {
        // The shortcut is `# `, `## `, `### ` — the trailing space is
        // load-bearing. A bare `#` line stays a paragraph.
        let blocks = parse("#\n");
        assert_eq!(blocks, vec![BlockSpec::paragraph("#")]);
    }

    #[test]
    fn serialize_clamps_heading_level() {
        // BlockSpec::heading constructor clamps, but be defensive:
        // a level-99 value should still serialize as ### so the
        // output is at least parseable.
        let blocks = vec![BlockSpec::Heading {
            level: 99,
            text: "x".into(),
        }];
        assert_eq!(serialize(&blocks), "### x\n");
    }
}
