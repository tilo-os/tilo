//! Tiny HTML → [`ContentBlock`] converter for live-fetched article bodies.
//!
//! feed-rs hands us article content as an HTML fragment. We don't embed a
//! web engine in the reader pane — instead we flatten the markup into the
//! same block list the gemtext parser and the sample data produce, so the
//! reader has exactly one rendering path. This is intentionally lossy:
//! headings, paragraphs, list items, blockquotes and `<pre>` survive;
//! everything else collapses to text.

use crate::model::{BlockKind, ContentBlock};

/// Convert an HTML fragment into renderable blocks.
pub fn to_blocks(html: &str) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let mut buf = String::new();
    let mut kind = BlockKind::Paragraph;
    let mut in_pre = false;
    let mut in_tag = false;
    let mut tag = String::new();

    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                handle_tag(
                    &tag.to_ascii_lowercase(),
                    &mut blocks,
                    &mut buf,
                    &mut kind,
                    &mut in_pre,
                );
                tag.clear();
            } else {
                tag.push(c);
            }
            continue;
        }
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '&' => {
                let mut ent = String::new();
                while let Some(&n) = chars.peek() {
                    if n == ';' {
                        chars.next();
                        break;
                    }
                    if ent.len() > 10 || n == '<' || n == '&' {
                        break;
                    }
                    ent.push(n);
                    chars.next();
                }
                buf.push_str(&decode_entity(&ent));
            }
            _ => buf.push(c),
        }
    }
    // Final flush of any trailing text.
    flush_block(&mut blocks, &mut buf, &mut kind, in_pre);

    if blocks.is_empty() {
        let text = collapse_ws(&strip_all_tags(html));
        if !text.is_empty() {
            blocks.push(ContentBlock::new(BlockKind::Paragraph, text));
        }
    }
    blocks
}

fn handle_tag(
    tag: &str,
    blocks: &mut Vec<ContentBlock>,
    buf: &mut String,
    kind: &mut BlockKind,
    in_pre: &mut bool,
) {
    let name = tag
        .trim_start_matches('/')
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("");
    let closing = tag.starts_with('/');

    match name {
        "p" | "div" | "section" | "article" => {
            flush_block(blocks, buf, kind, *in_pre);
        }
        "br" => {
            buf.push('\n');
        }
        "h1" => set_block(blocks, buf, kind, closing, BlockKind::Heading1, *in_pre),
        "h2" => set_block(blocks, buf, kind, closing, BlockKind::Heading2, *in_pre),
        "h3" | "h4" | "h5" | "h6" => {
            set_block(blocks, buf, kind, closing, BlockKind::Heading3, *in_pre);
        }
        "li" => set_block(blocks, buf, kind, closing, BlockKind::Bullet, *in_pre),
        "blockquote" => set_block(blocks, buf, kind, closing, BlockKind::Quote, *in_pre),
        "pre" => {
            if closing {
                flush_block(blocks, buf, kind, true);
                *in_pre = false;
            } else {
                flush_block(blocks, buf, kind, false);
                *kind = BlockKind::Code;
                *in_pre = true;
            }
        }
        "hr" => {
            flush_block(blocks, buf, kind, *in_pre);
            blocks.push(ContentBlock::new(BlockKind::Divider, String::new()));
        }
        _ => {}
    }
}

/// Emit the buffered text using the active block kind, then restore the
/// parser's paragraph default. All tag and end-of-input boundaries share this
/// path so whitespace and `<pre>` handling cannot drift apart.
fn flush_block(
    blocks: &mut Vec<ContentBlock>,
    buf: &mut String,
    kind: &mut BlockKind,
    in_pre: bool,
) {
    let text = if in_pre {
        buf.trim_matches('\n').to_string()
    } else {
        collapse_ws(buf)
    };
    if !text.is_empty() {
        blocks.push(ContentBlock::new(*kind, text));
    }
    buf.clear();
    *kind = BlockKind::Paragraph;
}

fn set_block(
    blocks: &mut Vec<ContentBlock>,
    buf: &mut String,
    kind: &mut BlockKind,
    closing: bool,
    new_kind: BlockKind,
    in_pre: bool,
) {
    if closing {
        let text = if in_pre {
            buf.clone()
        } else {
            collapse_ws(buf)
        };
        if !text.is_empty() {
            blocks.push(ContentBlock::new(*kind, text));
        }
        buf.clear();
        *kind = BlockKind::Paragraph;
    } else {
        // Opening: flush prior text, switch kind.
        let text = collapse_ws(buf);
        if !text.is_empty() {
            blocks.push(ContentBlock::new(BlockKind::Paragraph, text));
        }
        buf.clear();
        *kind = new_kind;
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_all_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode_entity(ent: &str) -> String {
    match ent {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" | "#39" => "'".into(),
        "nbsp" | "#160" => " ".into(),
        "hellip" => "…".into(),
        "mdash" => "—".into(),
        "ndash" => "–".into(),
        "rsquo" | "#8217" => "’".into(),
        "lsquo" | "#8216" => "‘".into(),
        "ldquo" | "#8220" => "“".into(),
        "rdquo" | "#8221" => "”".into(),
        _ => {
            if let Some(num) = ent.strip_prefix('#') {
                let code = if let Some(hex) = num.strip_prefix('x') {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    num.parse::<u32>().ok()
                };
                if let Some(ch) = code.and_then(char::from_u32) {
                    return ch.to_string();
                }
            }
            format!("&{ent};")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTML renders paragraphs, headings, bullets, plain text, numeric entities and unclosed preformatted blocks.
    #[test]
    fn html_renders_blocks_text_and_entities() {
        // --- phase: paragraphs_and_headings ---
        {
            let html = "<h2>Title</h2><p>First para.</p><p>Second &amp; last.</p>";
            let blocks = to_blocks(html);
            assert_eq!(blocks.len(), 3);
            assert_eq!(blocks[0].kind, BlockKind::Heading2);
            assert_eq!(blocks[0].text, "Title");
            assert_eq!(blocks[2].text, "Second & last.");
        }

        // --- phase: list_items_become_bullets ---
        {
            let html = "<ul><li>One</li><li>Two</li></ul>";
            let blocks = to_blocks(html);
            let bullets = blocks
                .iter()
                .filter(|b| b.kind == BlockKind::Bullet)
                .count();
            assert_eq!(bullets, 2);
        }

        // --- phase: plain_text_fallback ---
        {
            let blocks = to_blocks("just some text without tags");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "just some text without tags");
        }

        // --- phase: numeric_entities ---
        {
            let blocks = to_blocks("<p>caf&#233; &#x41;</p>");
            assert_eq!(blocks[0].text, "café A");
        }

        // --- phase: unclosed_preformatted_block_uses_the_shared_final_flush ---
        {
            let blocks = to_blocks("<pre>\nfn main() {\n    work();\n}");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].kind, BlockKind::Code);
            assert_eq!(blocks[0].text, "fn main() {\n    work();\n}");
        }
    }
}
