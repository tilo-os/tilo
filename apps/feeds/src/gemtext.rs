//! Gemtext parser — the line-oriented markup of the Gemini protocol.
//!
//! Produces the same [`ContentBlock`] list as the HTML converter so the
//! reader pane renders gemini capsules natively. Spec:
//! <https://gemini.circumlunar.space/docs/gemtext.gmi>.
//!
//! Live `gemini://` fetching lives in [`crate::fetch`] (TLS over port 1965,
//! TOFU). The parser itself is pure + offline so it can render the bundled
//! sample capsule in `--shots`.

use crate::model::{BlockKind, ContentBlock};

/// Parse a gemtext document into renderable blocks.
pub fn to_blocks(text: &str) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let mut in_pre = false;
    let mut pre_buf: Vec<&str> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_pre {
                // Closing fence — emit the accumulated code block.
                blocks.push(ContentBlock::new(BlockKind::Code, pre_buf.join("\n")));
                pre_buf.clear();
                in_pre = false;
            } else {
                in_pre = true;
                let _ = rest; // alt-text label; ignored for rendering
            }
            continue;
        }
        if in_pre {
            pre_buf.push(line);
            continue;
        }

        if let Some(rest) = line.strip_prefix("=>") {
            let rest = rest.trim_start();
            // `=> url optional label`
            let (url, label) = match rest.split_once(char::is_whitespace) {
                Some((u, l)) => (u.trim(), l.trim()),
                None => (rest.trim(), ""),
            };
            let text = if label.is_empty() { url } else { label };
            blocks.push(ContentBlock::link(text, url));
        } else if let Some(rest) = line.strip_prefix("###") {
            blocks.push(ContentBlock::new(BlockKind::Heading3, rest.trim()));
        } else if let Some(rest) = line.strip_prefix("##") {
            blocks.push(ContentBlock::new(BlockKind::Heading2, rest.trim()));
        } else if let Some(rest) = line.strip_prefix('#') {
            blocks.push(ContentBlock::new(BlockKind::Heading1, rest.trim()));
        } else if let Some(rest) = line.strip_prefix("* ") {
            blocks.push(ContentBlock::new(BlockKind::Bullet, rest.trim()));
        } else if let Some(rest) = line.strip_prefix('>') {
            blocks.push(ContentBlock::new(BlockKind::Quote, rest.trim()));
        } else if line.trim().is_empty() {
            // Blank line — paragraph separator, nothing to emit.
        } else {
            blocks.push(ContentBlock::new(BlockKind::Paragraph, line.trim()));
        }
    }
    // Unterminated preformat block — emit what we have.
    if in_pre && !pre_buf.is_empty() {
        blocks.push(ContentBlock::new(BlockKind::Code, pre_buf.join("\n")));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gemtext renders headings, links, lists and preformatted blocks.
    #[test]
    fn gemtext_renders_headings_links_lists_and_preformatted() {
        // --- phase: headings_links_and_lists ---
        {
            let doc = "# Title\n## Sub\nA paragraph.\n* item one\n* item two\n=> gemini://example.org Capsule\n=> gemini://bare.org";
            let blocks = to_blocks(doc);
            assert_eq!(blocks[0].kind, BlockKind::Heading1);
            assert_eq!(blocks[1].kind, BlockKind::Heading2);
            assert_eq!(blocks[2].kind, BlockKind::Paragraph);
            assert_eq!(blocks[3].kind, BlockKind::Bullet);
            let link = blocks.iter().find(|b| b.kind == BlockKind::Link).unwrap();
            assert_eq!(link.text, "Capsule");
            assert_eq!(link.href.as_deref(), Some("gemini://example.org"));
            // Bare link uses the URL as its label.
            let bare = blocks.last().unwrap();
            assert_eq!(bare.text, "gemini://bare.org");
        }

        // --- phase: preformatted_blocks ---
        {
            let doc = "```rust\nfn main() {}\nlet x = 1;\n```\nafter";
            let blocks = to_blocks(doc);
            assert_eq!(blocks[0].kind, BlockKind::Code);
            assert_eq!(blocks[0].text, "fn main() {}\nlet x = 1;");
            assert_eq!(blocks[1].text, "after");
        }
    }
}
