//! Pure functions to translate between a page's markdown body on
//! disk and the in-memory block list the [`ui::widget::BlockEditor`]
//! renders.
//!
//! Phase 3 extends this with four more block kinds: bullet lists,
//! checklists, fenced code blocks, and dividers. The grammar is:
//!
//! - A line beginning with `# `, `## `, `### ` is a heading.
//! - A line beginning with `- ` / `* ` (after optional 2-space
//!   indent groups, max 5 levels) is a bullet list item.
//! - A line beginning with `- [ ] ` / `- [x] ` (similarly indentable)
//!   is a checklist item.
//! - A line equal to `---` is a divider.
//! - A line beginning with ` ``` ` opens a fenced code block; the
//!   matching closing fence terminates it. **v0.1 limitation**: code
//!   block body is single-line per block (see
//!   `ui::widget::BlockKind::CodeBlock` for the rationale).
//! - Any other non-empty line is a paragraph.
//! - Blank lines are block separators (consecutive blanks collapse).
//!
//! The serializer is the inverse: each block emits its canonical
//! prefix, blocks are joined by a single blank line, and the output
//! ends in a trailing newline. Round-trip is byte-stable for any
//! block list this module can produce.
//!
//! We intentionally re-derive the same grammar inside
//! [`ui::widget::BlockEditor::from_markdown`] / `to_markdown` so the
//! UI crate has no notes-specific knowledge — the duplication is the
//! seam, not the bug. This module is the right shape for the
//! markdown-only persistence loop the notes app uses; if a non-UI
//! consumer ever needs the block list, they reach for this module,
//! not the widget. Phase 4's inline marks will force consolidation;
//! until then, the two grammars stay byte-stable identical and any
//! drift surfaces in `markdown_phase3_roundtrip.rs`.

/// Storage-friendly block representation. Carries the kind tag + the
/// raw text content, with no UI / focus / cursor / RichTextLine
/// state. Parallel to `ui::widget::Block` but trivially clonable +
/// debuggable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSpec {
    Paragraph(String),
    /// 1, 2, or 3. Clamped on construction.
    Heading { level: u8, text: String },
    /// Unordered list item. `indent` clamped to [0, 5].
    Bullet { indent: u8, text: String },
    /// Checklist item. `indent` clamped to [0, 5].
    Checklist { indent: u8, done: bool, text: String },
    /// Fenced code block. `lang` is `None` for a bare ``` fence.
    CodeBlock { lang: Option<String>, text: String },
    /// Horizontal rule. Carries no body.
    Divider,
}

/// Same constant as [`ui::widget::block_editor::MAX_LIST_INDENT`].
/// Re-declared here so the storage module doesn't take a UI dependency
/// just for the integer — the round-trip test asserts they stay in sync.
pub const MAX_LIST_INDENT: u8 = 5;

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
    pub fn bullet(indent: u8, s: impl Into<String>) -> Self {
        BlockSpec::Bullet {
            indent: indent.min(MAX_LIST_INDENT),
            text: s.into(),
        }
    }
    pub fn checklist(indent: u8, done: bool, s: impl Into<String>) -> Self {
        BlockSpec::Checklist {
            indent: indent.min(MAX_LIST_INDENT),
            done,
            text: s.into(),
        }
    }
    pub fn code_block(lang: Option<String>, s: impl Into<String>) -> Self {
        BlockSpec::CodeBlock {
            lang,
            text: s.into(),
        }
    }
    pub fn divider() -> Self {
        BlockSpec::Divider
    }
}

/// Parse the leading indent of a list-item line. Mirror of the same
/// helper in `ui::widget::block_editor` — kept private here so the
/// two grammars stay aligned without taking a UI dependency.
fn parse_list_indent(line: &str) -> (u8, &str) {
    let mut bytes = 0usize;
    let mut level: u8 = 0;
    let b = line.as_bytes();
    while level < MAX_LIST_INDENT {
        if b.len() >= bytes + 2 && &b[bytes..bytes + 2] == b"  " {
            bytes += 2;
            level += 1;
        } else if b.len() > bytes && b[bytes] == b'\t' {
            bytes += 1;
            level += 1;
        } else {
            break;
        }
    }
    (level, &line[bytes..])
}

fn push_indent(out: &mut String, indent: u8) {
    for _ in 0..indent.min(MAX_LIST_INDENT) {
        out.push_str("  ");
    }
}

/// Parse a markdown string into a flat list of [`BlockSpec`]s.
///
/// Empty input → a single empty paragraph (so callers always have
/// somewhere to start typing).
pub fn parse(md: &str) -> Vec<BlockSpec> {
    let mut blocks = Vec::new();
    let mut lines = md.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        // Fenced code block.
        if let Some(rest) = line.strip_prefix("```") {
            let lang = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            let mut body: Vec<&str> = Vec::new();
            while let Some(next) = lines.peek() {
                if *next == "```" {
                    lines.next();
                    break;
                }
                body.push(lines.next().unwrap());
            }
            blocks.push(BlockSpec::code_block(lang, body.join("\n")));
            continue;
        }
        if line == "---" {
            blocks.push(BlockSpec::divider());
            continue;
        }
        // Lists. Checklist must be matched BEFORE bullet because
        // `- [ ]` starts with `- `.
        let (indent, body) = parse_list_indent(line);
        if let Some(check_body) = body
            .strip_prefix("- [ ] ")
            .or_else(|| body.strip_prefix("* [ ] "))
        {
            blocks.push(BlockSpec::checklist(indent, false, check_body));
            continue;
        }
        if let Some(check_body) = body
            .strip_prefix("- [x] ")
            .or_else(|| body.strip_prefix("* [x] "))
            .or_else(|| body.strip_prefix("- [X] "))
            .or_else(|| body.strip_prefix("* [X] "))
        {
            blocks.push(BlockSpec::checklist(indent, true, check_body));
            continue;
        }
        if let Some(bullet_body) =
            body.strip_prefix("- ").or_else(|| body.strip_prefix("* "))
        {
            blocks.push(BlockSpec::bullet(indent, bullet_body));
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
            BlockSpec::Bullet { indent, text } => {
                push_indent(&mut out, *indent);
                out.push_str("- ");
                out.push_str(text);
            }
            BlockSpec::Checklist { indent, done, text } => {
                push_indent(&mut out, *indent);
                out.push_str(if *done { "- [x] " } else { "- [ ] " });
                out.push_str(text);
            }
            BlockSpec::CodeBlock { lang, text } => {
                out.push_str("```");
                if let Some(l) = lang {
                    out.push_str(l);
                }
                out.push('\n');
                out.push_str(text);
                out.push_str("\n```");
            }
            BlockSpec::Divider => {
                out.push_str("---");
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
