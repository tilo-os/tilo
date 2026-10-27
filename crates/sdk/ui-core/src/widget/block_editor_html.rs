//! Formatted text as HTML, both ways: what a copy from the editor offers as
//! `text/html`, and what a paste from a browser, an office suite or another
//! Tilo editor brings in. Paragraphs, headings, lists, checklists, quotes,
//! callouts, code, tables and rules keep their kind; bold, italic,
//! underline, strikethrough, inline code and links keep their marks. Any
//! other markup gives its words and nothing else: no script, style or
//! attribute survives a paste.

use std::fmt::Write as _;

use super::{BlockContent, BlockKind, CalloutKind, MAX_LIST_INDENT};
use crate::widget::marks::{Mark, MarkSpan, add_mark_to_range};

/// The most blocks one paste brings in. The HTML is already bounded by the
/// clipboard's transport limit; this bounds what a pathological document of
/// empty elements makes of it.
const MAX_PASTED_BLOCKS: usize = 20_000;

/// The scheme a link to a Tilo page is written with.
const PAGE_SCHEME: &str = "tilo-page:";

/// The scheme a link to a resource of this desktop is written with, the one
/// the native link uses, so a resource link survives a copy as HTML.
const RESOURCE_LINK_SCHEME: &str = "tilo://open#";

/// `target` is an address a resource of this desktop answers, rather than a
/// web page: a link written with it opens the resource where it lives.
pub fn is_resource_link(target: &str) -> bool {
    target
        .strip_prefix(RESOURCE_LINK_SCHEME)
        .is_some_and(|route| !route.trim().is_empty())
}

// ===========================================================================
// Blocks to HTML
// ===========================================================================

/// `blocks` as an HTML fragment: what a formatted copy offers.
#[must_use]
pub fn blocks_to_html(blocks: &[BlockContent]) -> String {
    let mut out = String::from("<meta charset=\"utf-8\">");
    // The lists open around the current block: whether each is numbered.
    let mut lists: Vec<bool> = Vec::new();
    let mut index = 0;
    while let Some(block) = blocks.get(index) {
        let list = match block.kind {
            BlockKind::Bullet { indent } | BlockKind::Checklist { indent, .. } => {
                Some((false, indent))
            }
            BlockKind::Numbered { indent } => Some((true, indent)),
            _ => None,
        };
        match list {
            Some((ordered, indent)) => {
                let depth = usize::from(indent) + 1;
                while lists.len() > depth {
                    close_list(&mut out, &mut lists);
                }
                if lists.len() == depth && lists.last() != Some(&ordered) {
                    close_list(&mut out, &mut lists);
                }
                while lists.len() < depth {
                    lists.push(ordered);
                    out.push_str(if ordered { "<ol>" } else { "<ul>" });
                }
                out.push_str("<li>");
                if let BlockKind::Checklist { done, .. } = block.kind {
                    out.push_str(if done {
                        "<input type=\"checkbox\" checked disabled> "
                    } else {
                        "<input type=\"checkbox\" disabled> "
                    });
                }
                push_inline(&mut out, &block.text, &block.marks);
                out.push_str("</li>");
                index += 1;
                continue;
            }
            None => {
                while !lists.is_empty() {
                    close_list(&mut out, &mut lists);
                }
            }
        }
        match &block.kind {
            BlockKind::CodeBlock { lang } => {
                // The lines of one fence are one `pre`.
                let mut end = index + 1;
                while blocks.get(end).is_some_and(|next| {
                    matches!(next.kind, BlockKind::CodeBlock { .. })
                        && next.code_group == block.code_group
                        && block.code_group.is_some()
                }) {
                    end += 1;
                }
                out.push_str("<pre><code");
                if let Some(lang) = lang.as_deref().filter(|lang| !lang.is_empty()) {
                    let _ = write!(out, " class=\"language-{}\"", escape_attr(lang));
                }
                out.push('>');
                for (line, code) in blocks[index..end].iter().enumerate() {
                    if line > 0 {
                        out.push('\n');
                    }
                    out.push_str(&escape_text(&code.text));
                }
                out.push_str("</code></pre>");
                index = end;
                continue;
            }
            BlockKind::Paragraph => {
                out.push_str("<p>");
                push_inline(&mut out, &block.text, &block.marks);
                out.push_str("</p>");
            }
            BlockKind::Heading(level) => {
                let level = (*level).clamp(1, 3);
                let _ = write!(out, "<h{level}>");
                push_inline(&mut out, &block.text, &block.marks);
                let _ = write!(out, "</h{level}>");
            }
            BlockKind::Quote => {
                out.push_str("<blockquote><p>");
                push_inline(&mut out, &block.text, &block.marks);
                out.push_str("</p></blockquote>");
            }
            BlockKind::Callout(kind) => {
                let _ = write!(out, "<blockquote data-callout=\"{}\"><p>", kind.name());
                push_inline(&mut out, &block.text, &block.marks);
                out.push_str("</p></blockquote>");
            }
            BlockKind::Divider => out.push_str("<hr>"),
            BlockKind::Table { .. } => push_table(&mut out, &block.text),
            BlockKind::Bullet { .. } | BlockKind::Numbered { .. } | BlockKind::Checklist { .. } => {
            }
        }
        index += 1;
    }
    while !lists.is_empty() {
        close_list(&mut out, &mut lists);
    }
    out
}

fn close_list(out: &mut String, lists: &mut Vec<bool>) {
    if let Some(ordered) = lists.pop() {
        out.push_str(if ordered { "</ol>" } else { "</ul>" });
    }
}

fn push_table(out: &mut String, source: &str) {
    let rows = super::markdown_table_data_rows(source);
    out.push_str("<table>");
    for (index, row) in rows.iter().enumerate() {
        out.push_str("<tr>");
        let cell = if index == 0 { "th" } else { "td" };
        for text in row {
            let _ = write!(out, "<{cell}>{}</{cell}>", escape_text(text.trim()));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

/// The tag a mark opens and closes, or `None` for a mark HTML carries no
/// meaning of (a transclusion).
fn mark_tags(mark: &Mark) -> Option<(String, &'static str)> {
    Some(match mark {
        Mark::Bold => ("<strong>".into(), "</strong>"),
        Mark::Italic => ("<em>".into(), "</em>"),
        Mark::Underline => ("<u>".into(), "</u>"),
        Mark::Strikethrough => ("<s>".into(), "</s>"),
        Mark::Code => ("<code>".into(), "</code>"),
        Mark::ExternalLink(url) => (format!("<a href=\"{}\">", escape_attr(url)), "</a>"),
        // A resource link keeps its own address, so HTML carries it as the
        // link it is.
        Mark::ResourceLink(href) => (format!("<a href=\"{}\">", escape_attr(href)), "</a>"),
        Mark::Link(page) => (format!("<a href=\"{PAGE_SCHEME}{page}\">"), "</a>"),
        Mark::Mention(id) => (
            format!("<span data-mention=\"{}\">", escape_attr(id)),
            "</span>",
        ),
        Mark::Transclusion(_) => return None,
    })
}

/// `text` with its marks as nested tags. Marks may overlap however they
/// like, so the tags open and close at every boundary between runs of one
/// set of marks: always well nested.
fn push_inline(out: &mut String, text: &str, marks: &[MarkSpan]) {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for span in marks {
        cuts.push(span.range.start.min(text.len()));
        cuts.push(span.range.end.min(text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let Some(run) = text.get(start..end) else {
            continue;
        };
        if run.is_empty() {
            continue;
        }
        let open: Vec<(String, &str)> = marks
            .iter()
            .filter(|span| span.range.start <= start && span.range.end >= end)
            .filter_map(|span| mark_tags(&span.mark))
            .collect();
        for (tag, _) in &open {
            out.push_str(tag);
        }
        out.push_str(&escape_text(run).replace('\n', "<br>"));
        for (_, close) in open.iter().rev() {
            out.push_str(close);
        }
    }
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

// ===========================================================================
// HTML to blocks
// ===========================================================================

/// The blocks `html` shows: what a formatted paste brings in. Empty when it
/// shows no words.
#[must_use]
pub fn blocks_from_html(html: &str) -> Vec<BlockContent> {
    let mut builder = Builder::default();
    let mut tokens = Tokens { rest: html };
    while let Some(token) = tokens.next() {
        if builder.blocks.len() >= MAX_PASTED_BLOCKS {
            break;
        }
        match token {
            Token::Text(text) => builder.text(&decode_entities(text)),
            Token::Start {
                name,
                attrs,
                closed,
            } => {
                if matches!(
                    name.as_str(),
                    "script" | "style" | "head" | "title" | "template"
                ) {
                    if !closed {
                        tokens.skip_past_end(&name);
                    }
                    continue;
                }
                builder.start(&name, attrs);
                if closed {
                    builder.end(&name);
                }
            }
            Token::End(name) => builder.end(&name),
        }
    }
    builder.finish()
}

enum Token<'a> {
    Text(&'a str),
    Start {
        name: String,
        attrs: &'a str,
        closed: bool,
    },
    End(String),
}

struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            if !self.rest.starts_with('<') {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let (text, rest) = self.rest.split_at(end);
                self.rest = rest;
                return Some(Token::Text(text));
            }
            if let Some(after) = self.rest.strip_prefix("<!--") {
                self.rest = after
                    .find("-->")
                    .and_then(|end| after.get(end + 3..))
                    .unwrap_or_default();
                continue;
            }
            if self.rest.starts_with("<!") || self.rest.starts_with("<?") {
                self.rest = self
                    .rest
                    .find('>')
                    .and_then(|end| self.rest.get(end + 1..))
                    .unwrap_or_default();
                continue;
            }
            let after = self.rest.get(1..).unwrap_or_default();
            let name = tag_name(after.strip_prefix('/').unwrap_or(after));
            let starts_tag = after.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '/');
            let end = tag_end(self.rest).filter(|_| starts_tag && !name.is_empty());
            let Some(end) = end else {
                // `a < b` written raw: the `<` is text.
                self.rest = after;
                return Some(Token::Text("<"));
            };
            let inner = self.rest.get(1..end).unwrap_or_default();
            self.rest = self.rest.get(end + 1..).unwrap_or_default();
            if inner.starts_with('/') {
                return Some(Token::End(name));
            }
            let attrs = inner.trim_start().get(name.len()..).unwrap_or_default();
            let closed = attrs.trim_end().ends_with('/') || is_void(&name);
            return Some(Token::Start {
                name,
                attrs,
                closed,
            });
        }
    }

    /// Skip a raw-text element's content (a script, a style).
    fn skip_past_end(&mut self, name: &str) {
        let close = format!("</{name}");
        let lower = self.rest.to_ascii_lowercase();
        // Lowercasing ASCII keeps every byte where it was.
        let rest = self.rest;
        self.rest = lower
            .find(&close)
            .and_then(|at| rest.get(at..))
            .and_then(|after| after.get(after.find('>')? + 1..))
            .unwrap_or_default();
    }
}

/// Where the tag `rest` starts with ends: its `>`, outside quoted values.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote = None;
    for (at, ch) in rest.char_indices().skip(1) {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(open), _) if ch == open => quote = None,
            (None, '>') => return Some(at),
            _ => {}
        }
    }
    None
}

fn tag_name(inner: &str) -> String {
    inner
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr" | "img" | "input" | "meta" | "link" | "col" | "wbr" | "area" | "base"
    )
}

/// The value of attribute `key` in a tag's attributes, entities decoded.
fn attr(attrs: &str, key: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '/');
        if rest.is_empty() {
            return None;
        }
        let name_end = rest
            .find(|ch: char| ch.is_whitespace() || ch == '=' || ch == '/')
            .unwrap_or(rest.len());
        let name = rest
            .get(..name_end)
            .unwrap_or_default()
            .to_ascii_lowercase();
        rest = rest.get(name_end..).unwrap_or_default().trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let quoted = after.strip_prefix(['"', '\'']).zip(after.chars().next());
            let (value, next) = if let Some((body, quote)) = quoted {
                let end = body.find(quote).unwrap_or(body.len());
                (
                    body.get(..end).unwrap_or_default(),
                    body.get(end + 1..).unwrap_or_default(),
                )
            } else {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                after.split_at_checked(end).unwrap_or((after, ""))
            };
            rest = next;
            value
        } else {
            ""
        };
        if name == key {
            return Some(decode_entities(value));
        }
    }
}

/// `text` with its character references resolved.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        rest = rest.get(at..).unwrap_or_default();
        let end = rest.find(';').filter(|end| *end <= 12);
        let decoded = end.and_then(|end| {
            let name = rest.get(1..end)?;
            let ch = if let Some(number) = name.strip_prefix('#') {
                let code = number.strip_prefix(['x', 'X']).map_or_else(
                    || number.parse().ok(),
                    |hex| u32::from_str_radix(hex, 16).ok(),
                )?;
                char::from_u32(code)?
            } else {
                match name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    "ndash" => '–',
                    "mdash" => '—',
                    "hellip" => '…',
                    "lsquo" => '‘',
                    "rsquo" => '’',
                    "ldquo" => '“',
                    "rdquo" => '”',
                    "bull" => '•',
                    "middot" => '·',
                    "copy" => '©',
                    "reg" => '®',
                    "trade" => '™',
                    "euro" => '€',
                    "times" => '×',
                    _ => return None,
                }
            };
            Some((ch, end))
        });
        let (ch, used) = decoded.map_or(('&', 1), |(ch, end)| (ch, end + 1));
        out.push(ch);
        rest = rest.get(used..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// An inline element open while the words go in: the mark it gives, and
/// where in the current block it began.
struct OpenInline {
    name: String,
    marks: Vec<Mark>,
    start: usize,
}

/// A table being read: its rows of cells.
#[derive(Default)]
struct TableRows {
    rows: Vec<Vec<String>>,
    cell: Option<String>,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<BlockContent>,
    /// The block the words are going into.
    current: Option<BlockContent>,
    inline: Vec<OpenInline>,
    /// The lists open: whether each is numbered.
    lists: Vec<bool>,
    /// How deep in quotes, and the callout the innermost one is.
    quotes: Vec<Option<CalloutKind>>,
    /// Inside a `pre`: its language, and the code read so far.
    pre: Option<(Option<String>, String)>,
    table: Option<TableRows>,
    /// The kind a new block inside the current list item takes.
    item: Option<BlockKind>,
    heading: Option<u8>,
}

impl Builder {
    fn kind_here(&self) -> BlockKind {
        if let Some(level) = self.heading {
            return BlockKind::Heading(level);
        }
        if let Some(kind) = &self.item {
            return kind.clone();
        }
        match self.quotes.last() {
            Some(Some(callout)) => BlockKind::Callout(*callout),
            Some(None) => BlockKind::Quote,
            None => BlockKind::Paragraph,
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((_, code)) = self.pre.as_mut() {
            code.push_str(text);
            return;
        }
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.cell.as_mut() {
                push_collapsed(cell, text);
            }
            return;
        }
        let at_start = self
            .current
            .as_ref()
            .is_none_or(|block| block.text.is_empty());
        let collapsed: String = collapse_whitespace(text);
        let collapsed = if at_start {
            collapsed.trim_start().to_owned()
        } else {
            collapsed
        };
        if collapsed.is_empty() {
            return;
        }
        let kind = self.kind_here();
        let block = self
            .current
            .get_or_insert_with(|| BlockContent::paragraph("", "").with_kind(kind));
        if block.text.ends_with(' ') && collapsed.starts_with(' ') {
            block.text.push_str(collapsed.get(1..).unwrap_or_default());
        } else {
            block.text.push_str(&collapsed);
        }
    }

    /// End the current block: its open marks close at its end and reopen
    /// at the start of the next.
    fn flush(&mut self) {
        let Some(mut block) = self.current.take() else {
            return;
        };
        let trimmed = block.text.trim_end().len();
        block.text.truncate(trimmed);
        for open in &mut self.inline {
            let end = block.text.len();
            for mark in &open.marks {
                if open.start < end {
                    add_mark_to_range(&mut block.marks, open.start..end, mark.clone());
                }
            }
            open.start = 0;
        }
        if !block.text.is_empty() || matches!(block.kind, BlockKind::Checklist { .. }) {
            self.blocks.push(block);
        }
    }

    fn start(&mut self, name: &str, attrs: &str) {
        match name {
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "aside"
            | "figure" | "figcaption" | "dt" | "dd" => self.flush(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                let level = name
                    .get(1..)
                    .unwrap_or_default()
                    .parse::<u8>()
                    .unwrap_or(1)
                    .clamp(1, 3);
                self.heading = Some(level);
            }
            "br" => {
                if let Some((_, code)) = self.pre.as_mut() {
                    code.push('\n');
                } else if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
                    cell.push(' ');
                } else {
                    // A line break inside a block starts the next line as
                    // its own block of the same kind.
                    let kind = self.current.as_ref().map(|block| block.kind.clone());
                    self.flush();
                    if let Some(kind) = kind {
                        self.current = Some(BlockContent::paragraph("", "").with_kind(kind));
                    }
                }
            }
            "hr" => {
                self.flush();
                self.blocks
                    .push(BlockContent::paragraph("", "").with_kind(BlockKind::Divider));
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.push(name == "ol");
            }
            "li" => {
                self.flush();
                let indent = u8::try_from(self.lists.len().saturating_sub(1))
                    .unwrap_or(MAX_LIST_INDENT)
                    .min(MAX_LIST_INDENT);
                self.item = Some(if self.lists.last() == Some(&true) {
                    BlockKind::Numbered { indent }
                } else {
                    BlockKind::Bullet { indent }
                });
            }
            "input" => {
                let checkbox =
                    attr(attrs, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("checkbox"));
                if checkbox
                    && let Some(BlockKind::Bullet { indent } | BlockKind::Numbered { indent }) =
                        self.item
                {
                    let done = attr(attrs, "checked").is_some();
                    let kind = BlockKind::Checklist { indent, done };
                    self.item = Some(kind.clone());
                    match self.current.as_mut() {
                        Some(block) => block.kind = kind,
                        None => {
                            self.current = Some(BlockContent::paragraph("", "").with_kind(kind))
                        }
                    }
                }
            }
            "blockquote" => {
                self.flush();
                let callout =
                    attr(attrs, "data-callout").and_then(|name| CalloutKind::from_name(&name));
                self.quotes.push(callout);
            }
            "pre" => {
                self.flush();
                self.pre = Some((language(attrs), String::new()));
            }
            "code" if self.pre.is_some() => {
                if let Some((lang @ None, _)) = self.pre.as_mut() {
                    *lang = language(attrs);
                }
            }
            "table" => {
                self.flush();
                self.table = Some(TableRows::default());
            }
            "tr" => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                }
            }
            "td" | "th" => {
                if let Some(table) = self.table.as_mut() {
                    table.cell = Some(String::new());
                }
            }
            _ => {
                let marks = inline_marks(name, attrs);
                let start = self.current.as_ref().map_or(0, |block| block.text.len());
                if !marks.is_empty() || matches!(name, "b" | "strong" | "span" | "font" | "a") {
                    self.inline.push(OpenInline {
                        name: name.to_owned(),
                        marks,
                        start,
                    });
                }
            }
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "aside"
            | "figure" | "figcaption" | "dt" | "dd" => self.flush(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.heading = None;
            }
            "li" => {
                self.flush();
                self.item = None;
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.pop();
                self.item = None;
            }
            "blockquote" => {
                self.flush();
                self.quotes.pop();
            }
            "pre" => {
                if let Some((lang, code)) = self.pre.take() {
                    self.push_code(lang, &code);
                }
            }
            "td" | "th" => {
                if let Some(table) = self.table.as_mut()
                    && let Some(cell) = table.cell.take()
                {
                    match table.rows.last_mut() {
                        Some(row) => row.push(cell.trim().to_owned()),
                        None => table.rows.push(vec![cell.trim().to_owned()]),
                    }
                }
            }
            "table" => {
                if let Some(table) = self.table.take() {
                    self.push_table(table);
                }
            }
            _ => {
                let Some(at) = self.inline.iter().rposition(|open| open.name == name) else {
                    return;
                };
                let open = self.inline.remove(at);
                if let Some(block) = self.current.as_mut() {
                    let end = block.text.len();
                    if open.start < end {
                        for mark in open.marks {
                            add_mark_to_range(&mut block.marks, open.start..end, mark);
                        }
                    }
                }
            }
        }
    }

    fn push_code(&mut self, lang: Option<String>, code: &str) {
        let code = code.strip_suffix('\n').unwrap_or(code);
        let group = uuid::Uuid::new_v4().to_string();
        for line in code.split('\n') {
            let mut block = BlockContent::paragraph("", line.trim_end_matches('\r'))
                .with_kind(BlockKind::CodeBlock { lang: lang.clone() });
            block.code_group = Some(group.clone());
            self.blocks.push(block);
        }
    }

    fn push_table(&mut self, table: TableRows) {
        let rows: Vec<Vec<String>> = table
            .rows
            .into_iter()
            .filter(|row| !row.is_empty())
            .collect();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let line = |cells: &[String]| {
            let mut line = String::from("|");
            for column in 0..columns {
                let cell = cells.get(column).map_or("", String::as_str);
                let _ = write!(line, " {} |", cell.replace('|', "\\|"));
            }
            line
        };
        let mut source = line(&rows[0]);
        source.push('\n');
        source.push('|');
        for _ in 0..columns {
            source.push_str(" --- |");
        }
        for row in &rows[1..] {
            source.push('\n');
            source.push_str(&line(row));
        }
        self.blocks
            .push(BlockContent::paragraph("", source).with_kind(BlockKind::Table { visual: true }));
    }

    fn finish(mut self) -> Vec<BlockContent> {
        if let Some((lang, code)) = self.pre.take() {
            self.push_code(lang, &code);
        }
        if let Some(table) = self.table.take() {
            self.push_table(table);
        }
        self.flush();
        self.blocks
    }
}

/// The language a `pre` or `code` names in its class (`language-rust`).
fn language(attrs: &str) -> Option<String> {
    attr(attrs, "class")?
        .split_whitespace()
        .find_map(|class| {
            class
                .strip_prefix("language-")
                .or_else(|| class.strip_prefix("lang-"))
        })
        .filter(|lang| !lang.is_empty())
        .map(str::to_owned)
}

/// The marks an inline element gives, by its name and its inline style
/// (office suites and Google Docs mark their runs with `style`).
fn inline_marks(name: &str, attrs: &str) -> Vec<Mark> {
    let mut marks = Vec::new();
    let style = attr(attrs, "style")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let style_value = |property: &str| {
        style.split(';').find_map(|declaration| {
            let (key, value) = declaration.split_once(':')?;
            (key.trim() == property).then(|| value.trim().to_owned())
        })
    };
    let weight = style_value("font-weight");
    let normal_weight = weight
        .as_deref()
        .is_some_and(|weight| weight == "normal" || weight.parse::<u16>().is_ok_and(|w| w < 600));
    let bold_weight = weight.as_deref().is_some_and(|weight| {
        weight == "bold" || weight == "bolder" || weight.parse::<u16>().is_ok_and(|w| w >= 600)
    });
    if (matches!(name, "b" | "strong") && !normal_weight) || bold_weight {
        marks.push(Mark::Bold);
    }
    let italic_style =
        style_value("font-style").is_some_and(|value| value == "italic" || value == "oblique");
    if matches!(name, "i" | "em" | "cite" | "var") || italic_style {
        marks.push(Mark::Italic);
    }
    let decoration = style_value("text-decoration")
        .or_else(|| style_value("text-decoration-line"))
        .unwrap_or_default();
    if name == "u" || name == "ins" || decoration.contains("underline") {
        marks.push(Mark::Underline);
    }
    if matches!(name, "s" | "strike" | "del") || decoration.contains("line-through") {
        marks.push(Mark::Strikethrough);
    }
    if matches!(name, "code" | "tt" | "kbd" | "samp") {
        marks.push(Mark::Code);
    }
    if name == "a"
        && let Some(href) = attr(attrs, "href")
    {
        let href = href.trim();
        if let Some(page) = href.strip_prefix(PAGE_SCHEME) {
            if let Ok(page) = page.parse() {
                marks.push(Mark::Link(page));
            }
        } else if href.starts_with(RESOURCE_LINK_SCHEME) {
            marks.push(Mark::ResourceLink(href.to_owned()));
        } else if href.starts_with("https://")
            || href.starts_with("http://")
            || href.starts_with("mailto:")
        {
            marks.push(Mark::ExternalLink(href.to_owned()));
        }
    }
    if name == "span"
        && let Some(id) = attr(attrs, "data-mention").filter(|id| !id.is_empty())
    {
        marks.push(Mark::Mention(id));
    }
    marks
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_collapsed(&mut out, text);
    out
}

/// Append `text` with each run of whitespace one space, as HTML shows it.
fn push_collapsed(out: &mut String, text: &str) {
    let mut space = out.ends_with(' ');
    for ch in text.chars() {
        // A no-break space is a space the reader typed: kept, as a space.
        if ch.is_whitespace() && ch != '\u{a0}' {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(if ch == '\u{a0}' { ' ' } else { ch });
            space = false;
        }
    }
}

trait WithKind {
    fn with_kind(self, kind: BlockKind) -> Self;
}

impl WithKind for BlockContent {
    fn with_kind(mut self, kind: BlockKind) -> Self {
        self.kind = kind;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, marks: Vec<MarkSpan>) -> BlockContent {
        let mut block = BlockContent::paragraph("", text);
        block.marks = marks;
        block
    }

    fn kinds(blocks: &[BlockContent]) -> Vec<(BlockKind, &str)> {
        blocks
            .iter()
            .map(|b| (b.kind.clone(), b.text.as_str()))
            .collect()
    }

    /// What the editor copies, it pastes back unchanged.
    #[test]
    fn the_editors_own_html_round_trips() {
        let group = Some("g".to_owned());
        let mut code_a =
            BlockContent::paragraph("", "fn main() {").with_kind(BlockKind::CodeBlock {
                lang: Some("rust".into()),
            });
        code_a.code_group.clone_from(&group);
        let mut code_b =
            BlockContent::paragraph("", "    a < b && c").with_kind(BlockKind::CodeBlock {
                lang: Some("rust".into()),
            });
        code_b.code_group = group;
        let blocks = vec![
            BlockContent::paragraph("", "Title").with_kind(BlockKind::Heading(1)),
            para(
                "plain bold italic both",
                vec![
                    MarkSpan::new(6..10, Mark::Bold),
                    MarkSpan::new(11..17, Mark::Italic),
                    MarkSpan::new(18..22, Mark::Bold),
                    MarkSpan::new(18..22, Mark::Italic),
                ],
            ),
            para(
                "a link & <tag>",
                vec![MarkSpan::new(
                    2..6,
                    Mark::ExternalLink("https://tilo.so/?a=1&b=\"2\"".into()),
                )],
            ),
            BlockContent::paragraph("", "one").with_kind(BlockKind::Bullet { indent: 0 }),
            BlockContent::paragraph("", "nested").with_kind(BlockKind::Bullet { indent: 1 }),
            BlockContent::paragraph("", "first").with_kind(BlockKind::Numbered { indent: 0 }),
            BlockContent::paragraph("", "done").with_kind(BlockKind::Checklist {
                indent: 0,
                done: true,
            }),
            BlockContent::paragraph("", "open").with_kind(BlockKind::Checklist {
                indent: 0,
                done: false,
            }),
            BlockContent::paragraph("", "said").with_kind(BlockKind::Quote),
            BlockContent::paragraph("", "careful")
                .with_kind(BlockKind::Callout(CalloutKind::Warning)),
            BlockContent::paragraph("", "").with_kind(BlockKind::Divider),
            code_a,
            code_b,
            para(
                "under strike code",
                vec![
                    MarkSpan::new(0..5, Mark::Underline),
                    MarkSpan::new(6..12, Mark::Strikethrough),
                    MarkSpan::new(13..17, Mark::Code),
                ],
            ),
        ];
        let html = blocks_to_html(&blocks);
        let back = blocks_from_html(&html);
        assert_eq!(kinds(&back), kinds(&blocks), "{html}");
        let sorted = |marks: &[MarkSpan]| {
            let mut marks: Vec<String> = marks
                .iter()
                .map(|span| format!("{:?} {:?}", span.range, span.mark))
                .collect();
            marks.sort();
            marks
        };
        for (got, want) in back.iter().zip(&blocks) {
            assert_eq!(
                sorted(&got.marks),
                sorted(&want.marks),
                "marks of {:?} in {html}",
                want.text
            );
        }
        assert_eq!(
            back[11].code_group, back[12].code_group,
            "one fence stays one"
        );
    }

    #[test]
    fn a_table_round_trips_through_html() {
        let table = BlockContent::paragraph("", "| a | b |\n| --- | --- |\n| 1 | 2 |")
            .with_kind(BlockKind::Table { visual: true });
        let html = blocks_to_html(std::slice::from_ref(&table));
        assert!(
            html.contains(
                "<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>"
            ),
            "{html}"
        );
        assert_eq!(kinds(&blocks_from_html(&html)), kinds(&[table]));
    }

    /// What browsers and office suites put on the clipboard pastes as the
    /// words with their formatting, and nothing else.
    #[test]
    fn foreign_html_pastes_its_words_and_formatting_only() {
        // Google Docs: a non-bold `b` wrapper, weight and slant in styles.
        let docs = r#"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-1"><p dir="ltr"><span style="font-weight:700;">Bold</span><span style="font-weight:400;"> and </span><span style="font-style:italic;">italic</span></p><ul><li><p><span>item</span></p></li></ul></b>"#;
        let blocks = blocks_from_html(docs);
        assert_eq!(
            kinds(&blocks),
            [
                (BlockKind::Paragraph, "Bold and italic"),
                (BlockKind::Bullet { indent: 0 }, "item"),
            ]
        );
        assert_eq!(
            blocks[0].marks,
            [
                MarkSpan::new(0..4, Mark::Bold),
                MarkSpan::new(9..15, Mark::Italic)
            ]
        );

        // A web page: scripts, styles, comments and attributes go; entities
        // resolve; whitespace collapses; a line break splits the block.
        let page = "<html><head><style>p { color: red }</style><title>x</title></head><body><!--StartFragment--><script>alert(1)</script><p onclick=\"evil()\">Caf&#233; &amp; t&#233;a&nbsp;&#x1F600;\n   more<br>next</p><pre><code class=\"language-py\">if a:\n    b()\n</code></pre><!--EndFragment--></body></html>";
        let blocks = blocks_from_html(page);
        assert_eq!(
            kinds(&blocks),
            [
                (BlockKind::Paragraph, "Café & téa 😀 more"),
                (BlockKind::Paragraph, "next"),
                (
                    BlockKind::CodeBlock {
                        lang: Some("py".into())
                    },
                    "if a:"
                ),
                (
                    BlockKind::CodeBlock {
                        lang: Some("py".into())
                    },
                    "    b()"
                ),
            ]
        );
        assert!(blocks.iter().all(|b| !b.text.contains("alert")));

        // A link to anything but the web or mail keeps its words, not its
        // target.
        let blocks = blocks_from_html(
            "<a href=\"javascript:alert(1)\">click</a> <a href=\"https://tilo.so\">site</a>",
        );
        assert_eq!(blocks[0].text, "click site");
        assert_eq!(
            blocks[0].marks,
            [MarkSpan::new(
                6..10,
                Mark::ExternalLink("https://tilo.so".into())
            )]
        );

        assert!(blocks_from_html("<p>  </p><div></div>").is_empty());
        assert_eq!(
            blocks_from_html("a < b").first().map(|b| b.text.as_str()),
            Some("a < b")
        );
    }
}
