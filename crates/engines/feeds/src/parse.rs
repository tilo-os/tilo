//! Reading what a site sent: a feed's articles, a capsule's page, and the text of an article's
//! body as Markdown the app keeps as a document.
//!
//! A feed carries its articles' bodies as HTML (or plain text); a capsule carries Gemtext. Both
//! are flattened into the same Markdown: headings, paragraphs, lists, quotes, code and rules
//! survive, links survive, and everything else becomes its text. That one shape is what the app's
//! reader draws, so HTML and Gemtext reach a person looking the same.

use std::io::Cursor;

use crate::{Article, Failure, Found};

/// The most summary characters a row reads before it shows the whole.
const SUMMARY_CHARS: usize = 240;

/// Read a feed — RSS, Atom or JSON — as the articles it lists.
///
/// # Errors
///
/// The bytes are not a feed this reader knows.
pub fn feed(bytes: &[u8], url: &str) -> Result<Found, Failure> {
    let parsed = feed_rs::parser::parse(Cursor::new(bytes))
        .map_err(|e| Failure::not_a_feed(format!("parse: {e}")))?;
    let title = parsed.title.map(|t| t.content.trim().to_owned()).unwrap_or_default();
    let articles = parsed
        .entries
        .iter()
        .map(|entry| {
            let title = entry
                .title
                .as_ref()
                .map(|t| t.content.trim().to_owned())
                .unwrap_or_default();
            // Atom links are unordered: a self link or an enclosure is not the article's web
            // page. Prefer an HTML alternate, then any alternate; keep the first link when the
            // entry names no alternate.
            let link = entry
                .links
                .iter()
                .filter(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
                .min_by_key(|link| match link.media_type.as_deref() {
                    Some("text/html" | "application/xhtml+xml") => 0,
                    None => 1,
                    _ => 2,
                })
                .or_else(|| entry.links.first())
                .map(|link| link.href.clone())
                .unwrap_or_default();
            let author = entry
                .authors
                .first()
                .map(|person| person.name.clone())
                .unwrap_or_default();
            let published = entry
                .published
                .or(entry.updated)
                .map_or(0, |at| at.timestamp());
            // Prefer the whole article; fall back to its summary.
            let summary_text = entry.summary.as_ref().map(|s| s.content.as_str());
            let body_text = entry
                .content
                .as_ref()
                .and_then(|c| c.body.as_deref())
                .or(summary_text)
                .unwrap_or_default();
            Article {
                title,
                author,
                url: link,
                published,
                summary: plain(summary_text.unwrap_or(body_text)),
                body: markdown(body_text),
            }
        })
        .collect();
    let _ = url;
    Ok(Found {
        title,
        pin: String::new(),
        articles,
    })
}

/// Read a capsule's Gemtext as the one article it is.
pub fn gemtext(text: &str, url: &str) -> Found {
    let body = gemtext_markdown(text);
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|rest| rest.trim().to_owned())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| url.to_owned());
    let summary: String = plain_text(text).chars().take(200).collect();
    Found {
        title: String::new(),
        pin: String::new(),
        articles: vec![Article {
            title,
            author: String::new(),
            url: url.to_owned(),
            published: 0,
            summary,
            body,
        }],
    }
}

/// A fragment of HTML as Markdown: its headings, paragraphs, list items, quotes, preformatted
/// text and rules, and nothing else.
pub fn markdown(html: &str) -> String {
    to_blocks(html)
        .into_iter()
        .map(|block| match block.kind {
            BlockKind::Heading1 => format!("# {}", escape(&block.text)),
            BlockKind::Heading2 => format!("## {}", escape(&block.text)),
            BlockKind::Heading3 => format!("### {}", escape(&block.text)),
            BlockKind::Paragraph => escape(&block.text),
            BlockKind::Bullet => format!("- {}", escape(&block.text)),
            BlockKind::Quote => format!("> {}", escape(&block.text)),
            BlockKind::Code => format!("```\n{}\n```", block.text),
            BlockKind::Link => match block.href.as_deref() {
                Some(href) if !href.is_empty() => {
                    format!("[{}]({})", escape(&block.text), escape(href))
                }
                _ => escape(&block.text),
            },
            BlockKind::Divider => "---".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A fragment of HTML as the plain text a summary reads.
pub fn plain(html: &str) -> String {
    let text = to_blocks(html)
        .into_iter()
        .filter(|block| !matches!(block.kind, BlockKind::Divider | BlockKind::Code))
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join(" ");
    text.chars().take(SUMMARY_CHARS).collect::<String>().trim().to_owned()
}

/// A capsule's Gemtext as Markdown.
fn gemtext_markdown(text: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut in_pre = false;
    let mut pre_buf: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(_rest) = line.strip_prefix("```") {
            if in_pre {
                blocks.push(format!("```\n{}\n```", pre_buf.join("\n")));
                pre_buf.clear();
            }
            in_pre = !in_pre;
            continue;
        }
        if in_pre {
            pre_buf.push(line);
            continue;
        }
        if let Some(rest) = line.strip_prefix("=>") {
            let rest = rest.trim_start();
            let (url, label) = match rest.split_once(char::is_whitespace) {
                Some((u, l)) => (u.trim(), l.trim()),
                None => (rest.trim(), ""),
            };
            if !url.is_empty() {
                let label = if label.is_empty() { url } else { label };
                blocks.push(format!("[{}]({})", escape(label), escape(url)));
            }
        } else if let Some(rest) = line.strip_prefix("###") {
            push_block(&mut blocks, "###", rest);
        } else if let Some(rest) = line.strip_prefix("##") {
            push_block(&mut blocks, "##", rest);
        } else if let Some(rest) = line.strip_prefix('#') {
            push_block(&mut blocks, "#", rest);
        } else if let Some(rest) = line.strip_prefix("* ") {
            if !rest.trim().is_empty() {
                blocks.push(format!("- {}", escape(rest.trim())));
            }
        } else if let Some(rest) = line.strip_prefix('>') {
            if !rest.trim().is_empty() {
                blocks.push(format!("> {}", escape(rest.trim())));
            }
        } else if !line.trim().is_empty() {
            blocks.push(escape(line.trim()));
        }
    }
    if in_pre && !pre_buf.is_empty() {
        blocks.push(format!("```\n{}\n```", pre_buf.join("\n")));
    }
    blocks.join("\n\n")
}

fn push_block(blocks: &mut Vec<String>, marker: &str, rest: &str) {
    let text = rest.trim();
    if !text.is_empty() {
        blocks.push(format!("{marker} {}", escape(text)));
    }
}

/// The words of a text with its whitespace collapsed, from what a capsule says.
fn plain_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One block of a body, before it is written as Markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Heading1,
    Heading2,
    Heading3,
    Paragraph,
    Bullet,
    Quote,
    Code,
    Link,
    Divider,
}

struct Block {
    kind: BlockKind,
    text: String,
    href: Option<String>,
}

impl Block {
    fn new(kind: BlockKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            href: None,
        }
    }
}

/// Flatten an HTML fragment into blocks, as the reader draws them: headings, paragraphs, list
/// items, quotes and `<pre>` survive and everything else becomes its text.
fn to_blocks(html: &str) -> Vec<Block> {
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
                let mut closed = false;
                while let Some(&n) = chars.peek() {
                    if n == ';' {
                        chars.next();
                        closed = true;
                        break;
                    }
                    if ent.len() > 10 || n == '<' || n == '&' {
                        break;
                    }
                    ent.push(n);
                    chars.next();
                }
                // Only `&name;` is an entity: a bare `&` stays as written, and so does what
                // follows it.
                if closed {
                    buf.push_str(&decode_entity(&ent));
                } else {
                    buf.push('&');
                    buf.push_str(&ent);
                }
            }
            _ => buf.push(c),
        }
    }
    flush_block(&mut blocks, &mut buf, &mut kind, in_pre);
    if blocks.is_empty() {
        let text = collapse_ws(&strip_all_tags(html));
        if !text.is_empty() {
            blocks.push(Block::new(BlockKind::Paragraph, text));
        }
    }
    blocks
}

fn handle_tag(
    tag: &str,
    blocks: &mut Vec<Block>,
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
        "p" | "div" | "section" | "article" => flush_block(blocks, buf, kind, *in_pre),
        "br" => buf.push('\n'),
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
            blocks.push(Block::new(BlockKind::Divider, String::new()));
        }
        _ => {}
    }
}

/// Emit the buffered text as the active block kind, then go back to a paragraph. Every tag and
/// end-of-input boundary shares this path, so whitespace and `<pre>` handling cannot drift.
fn flush_block(blocks: &mut Vec<Block>, buf: &mut String, kind: &mut BlockKind, in_pre: bool) {
    let text = if in_pre {
        buf.trim_matches('\n').to_owned()
    } else {
        collapse_ws(buf)
    };
    if !text.is_empty() {
        blocks.push(Block::new(*kind, text));
    }
    buf.clear();
    *kind = BlockKind::Paragraph;
}

fn set_block(
    blocks: &mut Vec<Block>,
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
            blocks.push(Block::new(*kind, text));
        }
        buf.clear();
        *kind = BlockKind::Paragraph;
    } else {
        let text = collapse_ws(buf);
        if !text.is_empty() {
            blocks.push(Block::new(BlockKind::Paragraph, text));
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

/// The character a named or numbered entity stands for.
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

/// Text that reads as written when a Markdown document has it: its markup characters escaped,
/// and a line that would start a block (a heading, a quote, a list) escaped at its start.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (at, line) in text.split('\n').enumerate() {
        if at > 0 {
            out.push('\n');
        }
        let mut chars = line.chars();
        if let Some(first) = chars.next() {
            let rest: String = chars.collect();
            let block_start = matches!(first, '#' | '>' | '-' | '+' | '*')
                || (first.is_ascii_digit() && rest.starts_with('.'));
            let whole = line;
            for (index, c) in whole.chars().enumerate() {
                if index == 0 && block_start {
                    out.push('\\');
                }
                match c {
                    '\\' => out.push_str("\\\\"),
                    '`' => out.push_str("\\`"),
                    '*' => out.push_str("\\*"),
                    '_' => out.push_str("\\_"),
                    '[' => out.push_str("\\["),
                    ']' => out.push_str("\\]"),
                    other => out.push(other),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
