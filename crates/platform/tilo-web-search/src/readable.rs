//! A web page as readable text: its main content in light Markdown, without
//! navigation, scripts, forms or page furniture.
//!
//! The reader keeps what a person reading the page would read — headings,
//! paragraphs, lists, quotes, tables, code blocks — and every link as
//! `[text](absolute address)`, because finding the right link is usually why
//! a page was fetched. Emphasis and images are dropped.

use url::Url;

use crate::html::{Document, NodeId, Selector, collapse_whitespace};

/// The most characters of text the reader produces for one page.
pub const MAX_TEXT_CHARS: usize = 1_000_000;

/// Elements whose content is never part of the reading.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object", "embed", "nav",
    "header", "footer", "aside", "form", "button", "select", "input", "textarea", "label",
    "dialog", "menu", "img", "picture", "video", "audio", "map", "head",
];
/// ARIA roles of page furniture.
const SKIPPED_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "dialog",
    "alertdialog",
    "menu",
    "menubar",
];
/// Class or id words of page furniture that is not marked up as such.
const SKIPPED_NAMES: &[&str] = &[
    "cookie",
    "sidebar",
    "navbox",
    "advert",
    "breadcrumb",
    "skip-link",
    "share",
];
/// Elements that start a new paragraph.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "address",
    "blockquote",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "details",
    "summary",
    "table",
    "caption",
    "center",
    "hgroup",
];

/// A page reduced to its reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readable {
    pub title: Option<String>,
    pub text: String,
}

/// Read `html`, served from `base`.
pub fn extract(html: &str, base: &Url) -> Readable {
    let doc = Document::parse(html);
    let title = title(&doc);
    let root = content_root(&doc);
    let mut renderer = Renderer {
        doc: &doc,
        base,
        out: String::new(),
        chars: 0,
        list_depth: 0,
        quote_depth: 0,
        in_cell: false,
    };
    renderer.node(root);
    Readable {
        title,
        text: tidy(&renderer.out),
    }
}

fn first(doc: &Document, css: &str) -> Option<NodeId> {
    let selector = Selector::parse(css).ok()?;
    doc.select_first(Document::ROOT, &selector)
}

fn title(doc: &Document) -> Option<String> {
    let from_title = first(doc, "title").map(|id| collapse_whitespace(&doc.text(id)));
    let from_meta = || {
        first(doc, "meta[property=og:title]")
            .and_then(|id| doc.attr(id, "content"))
            .map(collapse_whitespace)
    };
    from_title
        .filter(|t| !t.is_empty())
        .or_else(from_meta)
        .filter(|t| !t.is_empty())
}

/// The element holding the page's main content: its largest `<article>` when
/// that has real text, else `<main>`, else the body.
fn content_root(doc: &Document) -> NodeId {
    let text_len = |id: NodeId| collapse_whitespace(&doc.text(id)).len();
    if let Ok(article) = Selector::parse("article")
        && let Some((id, len)) = doc
            .select(Document::ROOT, &article)
            .into_iter()
            .map(|id| (id, text_len(id)))
            .max_by_key(|&(_, len)| len)
        && len >= 500
    {
        return id;
    }
    first(doc, "main")
        .or_else(|| first(doc, "[role=main]"))
        .or_else(|| first(doc, "body"))
        .unwrap_or(Document::ROOT)
}

struct Renderer<'a> {
    doc: &'a Document,
    base: &'a Url,
    out: String,
    chars: usize,
    list_depth: usize,
    quote_depth: usize,
    /// Inside a table cell, where a paragraph or line break would split the
    /// row across lines; breaks become spaces there.
    in_cell: bool,
}

impl Renderer<'_> {
    fn full(&self) -> bool {
        self.chars >= MAX_TEXT_CHARS
    }

    fn push(&mut self, s: &str) {
        if self.full() {
            return;
        }
        self.chars = self.chars.saturating_add(s.chars().count());
        self.out.push_str(s);
    }

    /// Inline text, whitespace collapsed against what is already written.
    fn text(&mut self, raw: &str) {
        let mut words = raw.split(|c: char| c.is_whitespace()).peekable();
        let leading_space = raw.starts_with(|c: char| c.is_whitespace());
        let trailing_space = raw.ends_with(|c: char| c.is_whitespace());
        if words.peek().is_none() || raw.trim().is_empty() {
            if !raw.is_empty() {
                self.space();
            }
            return;
        }
        if leading_space {
            self.space();
        }
        let mut first = true;
        for word in words.filter(|w| !w.is_empty()) {
            if !first {
                self.push(" ");
            }
            self.push(word);
            first = false;
        }
        if trailing_space {
            self.space();
        }
    }

    fn space(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with([' ', '\n']) {
            self.push(" ");
        }
    }

    /// End the current paragraph.
    fn block(&mut self) {
        if self.in_cell {
            self.space();
            return;
        }
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.push(if self.out.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            });
        }
        self.quote_prefix();
    }

    fn line(&mut self) {
        if self.in_cell {
            self.space();
            return;
        }
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.push("\n");
        }
        self.quote_prefix();
    }

    fn quote_prefix(&mut self) {
        for _ in 0..self.quote_depth {
            self.push("> ");
        }
    }

    fn skipped(&self, id: NodeId, name: &str) -> bool {
        if SKIPPED.contains(&name) {
            return true;
        }
        let doc = self.doc;
        if doc.attr(id, "hidden").is_some()
            || doc.attr(id, "aria-hidden") == Some("true")
            || doc
                .attr(id, "role")
                .is_some_and(|role| SKIPPED_ROLES.contains(&role))
        {
            return true;
        }
        if doc.attr(id, "style").is_some_and(|style| {
            let style: String = style.chars().filter(|c| !c.is_whitespace()).collect();
            style.contains("display:none") || style.contains("visibility:hidden")
        }) {
            return true;
        }
        let names = [doc.attr(id, "class"), doc.attr(id, "id")];
        names.into_iter().flatten().any(|value| {
            value
                .split_ascii_whitespace()
                .any(|word| SKIPPED_NAMES.iter().any(|n| word.eq_ignore_ascii_case(n)))
        })
    }

    fn children(&mut self, id: NodeId) {
        for &child in self.doc.children(id) {
            if self.full() {
                return;
            }
            self.node(child);
        }
    }

    fn node(&mut self, id: NodeId) {
        if let Some(text) = self.doc.text_node(id) {
            self.text(text);
            return;
        }
        let Some(name) = self.doc.name(id) else {
            // The root.
            self.children(id);
            return;
        };
        if self.skipped(id, name) {
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.heading(id, name),
            "a" => self.link(id),
            "br" => self.line(),
            "hr" => {
                self.block();
                self.push("---");
                self.block();
            }
            "pre" => self.preformatted(id),
            "code" | "kbd" | "samp" => {
                let code = collapse_whitespace(&self.doc.text(id));
                if !code.is_empty() {
                    self.space_before_inline();
                    self.push("`");
                    self.push(&code);
                    self.push("`");
                }
            }
            "ul" | "ol" => self.list(id, name == "ol"),
            "li" => {
                self.line();
                self.push("- ");
                self.children(id);
            }
            "blockquote" => {
                self.quote_depth = self.quote_depth.saturating_add(1);
                self.block();
                self.children(id);
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.block();
            }
            "tr" => {
                self.line();
                self.row(id);
            }
            "td" | "th" => self.children(id),
            _ if BLOCKS.contains(&name) => {
                self.block();
                self.children(id);
                self.block();
            }
            _ => self.children(id),
        }
    }

    fn space_before_inline(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with([' ', '\n', '(', '[']) {
            self.push(" ");
        }
    }

    fn heading(&mut self, id: NodeId, name: &str) {
        let text = collapse_whitespace(&self.doc.text(id));
        if text.is_empty() {
            return;
        }
        let level = name
            .strip_prefix('h')
            .and_then(|l| l.parse::<usize>().ok())
            .unwrap_or(1);
        self.block();
        self.push(&"#".repeat(level));
        self.push(" ");
        self.push(&text);
        self.block();
    }

    fn link(&mut self, id: NodeId) {
        let text = collapse_whitespace(&self.doc.text(id));
        let target = self
            .doc
            .attr(id, "href")
            .filter(|href| !href.starts_with('#'))
            .and_then(|href| self.base.join(href.trim()).ok())
            .filter(|url| matches!(url.scheme(), "http" | "https"));
        match (text.is_empty(), target) {
            (true, _) => {}
            (false, Some(url)) => {
                self.space_before_inline();
                self.push("[");
                self.push(&text.replace(['[', ']'], ""));
                self.push("](");
                self.push(url.as_str());
                self.push(")");
            }
            (false, None) => self.children(id),
        }
    }

    fn preformatted(&mut self, id: NodeId) {
        let code = self.doc.text(id);
        let code = code.trim_matches('\n');
        if code.trim().is_empty() {
            return;
        }
        self.block();
        self.push("```\n");
        self.push(code);
        self.push("\n```");
        self.block();
    }

    fn list(&mut self, id: NodeId, ordered: bool) {
        self.line();
        self.list_depth = self.list_depth.saturating_add(1);
        let mut number = 0usize;
        for &child in self.doc.children(id) {
            if self.full() {
                break;
            }
            if self.doc.name(child) == Some("li") {
                number = number.saturating_add(1);
                self.line();
                self.push(&"  ".repeat(self.list_depth.saturating_sub(1)));
                if ordered {
                    self.push(&format!("{number}. "));
                } else {
                    self.push("- ");
                }
                self.children(child);
            } else {
                self.node(child);
            }
        }
        self.list_depth = self.list_depth.saturating_sub(1);
        if self.list_depth == 0 {
            self.block();
        } else {
            self.line();
        }
    }

    fn row(&mut self, id: NodeId) {
        let start = self.out.len();
        let mut first = true;
        let outer = std::mem::replace(&mut self.in_cell, true);
        for &cell in self.doc.children(id) {
            if !matches!(self.doc.name(cell), Some("td" | "th")) {
                continue;
            }
            if !first {
                let end = self.out.trim_end_matches(' ').len();
                self.out.truncate(end);
                self.push(" | ");
            }
            first = false;
            self.children(cell);
        }
        self.in_cell = outer;
        // Trailing empty cells add nothing.
        let end = start.saturating_add(
            self.out
                .get(start..)
                .map_or(0, |row| row.trim_end_matches([' ', '|']).len()),
        );
        self.out.truncate(end);
        // A row of empty cells is layout, not content.
        if self
            .out
            .get(start..)
            .is_some_and(|row| row.chars().all(|c| c == '|' || c.is_whitespace()))
        {
            self.out.truncate(start);
        }
    }
}

/// Trim every line's trailing space and keep at most one blank line in a row.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim_start_matches("> ").trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push_str("\n\n");
        } else if !out.is_empty() {
            out.push('\n');
        }
        blank = false;
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(html: &str) -> Readable {
        extract(html, &Url::parse("https://example.org/docs/page").unwrap())
    }

    #[test]
    fn the_main_content_is_kept_and_page_furniture_dropped() {
        let page = read(
            r#"<html><head><title> Install  guide </title><style>p{}</style></head><body>
            <header><nav><a href="/">Home</a></nav></header>
            <div class="cookie">We use cookies</div>
            <main>
              <h1>Installing</h1>
              <p>Run the <code>install</code> command, then read
                 <a href="../faq#q1">the FAQ</a> or <a href="https://other.example/">elsewhere</a>.</p>
              <ul><li>one</li><li>two <a href="javascript:void(0)">x</a></li></ul>
              <pre>let x = 1;
  let y = 2;</pre>
              <table><tr><th>Key</th><th>Value</th></tr><tr><td>a</td><td>1</td></tr></table>
              <blockquote>Quoted words</blockquote>
              <p hidden>secret</p><script>alert(1)</script>
            </main>
            <footer>Copyright</footer></body></html>"#,
        );
        assert_eq!(page.title.as_deref(), Some("Install guide"));
        assert_eq!(
            page.text,
            "# Installing\n\n\
             Run the `install` command, then read [the FAQ](https://example.org/faq#q1) \
             or [elsewhere](https://other.example/).\n\n\
             - one\n\
             - two x\n\n\
             ```\nlet x = 1;\n  let y = 2;\n```\n\n\
             Key | Value\n\
             a | 1\n\n\
             > Quoted words"
        );
    }

    #[test]
    fn a_substantial_article_is_preferred_over_the_page_around_it() {
        let body = "word ".repeat(150);
        let page = read(&format!(
            "<body><div>Related links</div><article><h2>Story</h2><p>{body}</p></article>\
             <article><p>teaser</p></article></body>"
        ));
        assert!(page.text.starts_with("## Story\n\nword word"));
        assert!(!page.text.contains("Related links"));
        assert!(!page.text.contains("teaser"));
    }

    #[test]
    fn ordered_and_nested_lists_keep_their_shape() {
        let page =
            read("<body><ol><li>first<ul><li>inner</li></ul></li><li>second</li></ol></body>");
        assert_eq!(page.text, "1. first\n  - inner\n2. second");
    }

    #[test]
    fn a_table_row_stays_on_one_line() {
        let page = read(
            "<body><table><tr><td><p>src</p><p>dir</p></td><td><div>fix</div></td></tr>\
             <tr><td> </td><td></td></tr><tr><td>b</td><td>2</td></tr></table></body>",
        );
        assert_eq!(page.text, "src dir | fix\nb | 2");
    }
}
