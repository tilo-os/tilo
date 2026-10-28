//! An HTML mail as styled text.
//!
//! The reader shows a message's HTML part without an engine: the markup is
//! read into runs of text (bold, italic, headings, quotes, lists, links), and
//! nothing in it is ever fetched or run. Scripts, styles, hidden elements and
//! every attribute but a link's address are dropped, so there is no remote
//! content to block and no way for a message to tell the sender it was read.
//! Images show their alt text. Only web and mail addresses become links.

use html5ever::tendril::TendrilSink as _;
use html5ever::{Attribute, LocalName, ns};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// A cap on the markup read; the rest of a larger message is not shown.
const MAX_INPUT: usize = 2 * 1024 * 1024;
/// How deep nested block quotes are marked.
const MAX_QUOTE: usize = 4;

/// One stretch of text in one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// 1 for the largest heading to 3 for the smallest; 0 for body text.
    pub heading: u8,
    /// Part of a quotation.
    pub quote: bool,
    /// Where the text links, if it does.
    pub link: Option<String>,
}

impl Run {
    fn same_style(&self, other: &Run) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.heading == other.heading
            && self.quote == other.quote
            && self.link == other.link
    }
}

/// Elements whose content is never shown.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "textarea", "title", "template", "noscript", "object", "iframe",
    "svg", "math", "select",
];
/// Elements that end the line they sit in, and begin a new one.
const LINE: &[&str] = &[
    "br",
    "div",
    "tr",
    "li",
    "ul",
    "ol",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "address",
    "dl",
    "dt",
    "dd",
    "center",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "tbody",
    "thead",
    "tfoot",
];
/// Elements set apart by a blank line.
const PARAGRAPH: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "hr",
];
struct Writer {
    runs: Vec<Run>,
    /// Newlines owed before the next text: 1 for a line, 2 for a paragraph.
    owed: usize,
    /// Whether the last thing written ended a word's space.
    at_space: bool,
    bold: usize,
    italic: usize,
    heading: Vec<u8>,
    quote: usize,
    link: Vec<Option<String>>,
    pre: usize,
    /// Open lists: `Some(n)` numbers its items, `None` bullets them.
    lists: Vec<Option<usize>>,
}

impl Writer {
    fn new() -> Self {
        Self {
            runs: Vec::new(),
            owed: 0,
            at_space: true,
            bold: 0,
            italic: 0,
            heading: Vec::new(),
            quote: 0,
            link: Vec::new(),
            pre: 0,
            lists: Vec::new(),
        }
    }

    fn owe(&mut self, newlines: usize) {
        self.owed = self.owed.max(newlines);
    }

    fn style(&self) -> Run {
        Run {
            text: String::new(),
            bold: self.bold > 0 || !self.heading.is_empty(),
            italic: self.italic > 0,
            heading: self.heading.last().copied().unwrap_or(0),
            quote: self.quote > 0,
            link: self.link.last().cloned().flatten(),
        }
    }

    fn push(&mut self, mut run: Run) {
        if run.text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut()
            && last.same_style(&run)
        {
            last.text.push_str(&run.text);
            return;
        }
        run.text.shrink_to_fit();
        self.runs.push(run);
    }

    /// The line breaks owed, and the quote marks a new line begins with.
    fn begin_text(&mut self) {
        let started = !self.runs.is_empty();
        let newlines = if started { self.owed } else { 0 };
        if newlines > 0 {
            self.push(Run {
                text: "\n".repeat(newlines),
                ..self.style_plain()
            });
        }
        if (newlines > 0 || !started) && self.quote > 0 {
            let marks = "> ".repeat(self.quote.min(MAX_QUOTE));
            self.push(Run {
                text: marks,
                quote: true,
                ..Run::default()
            });
        }
        if newlines > 0 {
            self.at_space = true;
        }
        self.owed = 0;
    }

    /// Plain style for the breaks between lines, so they never underline.
    fn style_plain(&self) -> Run {
        Run {
            quote: self.quote > 0,
            ..Run::default()
        }
    }

    fn text(&mut self, raw: &str) {
        if self.pre > 0 {
            if raw.is_empty() {
                return;
            }
            self.begin_text();
            let run = Run {
                text: raw.to_owned(),
                ..self.style()
            };
            self.push(run);
            self.at_space = raw.ends_with(char::is_whitespace);
            return;
        }
        let mut out = String::with_capacity(raw.len());
        let mut at_space = self.at_space;
        for c in raw.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !at_space {
                    out.push(' ');
                    at_space = true;
                }
            } else {
                out.push(c);
                at_space = false;
            }
        }
        if out.trim().is_empty() {
            // Whitespace between elements: a space, never a line.
            if !self.runs.is_empty() && self.owed == 0 && !self.at_space {
                self.push(Run {
                    text: " ".into(),
                    ..self.style_plain()
                });
                self.at_space = true;
            }
            return;
        }
        self.begin_text();
        // `begin_text` may have started a line, so a leading space is not wanted.
        let text = if self.at_space {
            out.trim_start().to_owned()
        } else {
            out
        };
        self.at_space = at_space;
        let run = Run {
            text,
            ..self.style()
        };
        self.push(run);
    }

    fn item(&mut self) {
        self.owe(1);
        self.begin_text();
        let depth = self.lists.len().saturating_sub(1);
        let mut mark = "  ".repeat(depth);
        match self.lists.last_mut() {
            Some(Some(number)) => {
                *number += 1;
                mark.push_str(&number.to_string());
                mark.push_str(". ");
            }
            _ => mark.push_str("• "),
        }
        self.push(Run {
            text: mark,
            ..self.style_plain()
        });
        self.at_space = true;
    }
}

/// One step of the walk over the parsed document.
enum Step {
    Enter(Handle),
    Close(LocalName),
}

/// The value of the attribute `name`, as the parser decoded it.
fn attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.name.ns == ns!() && &*attribute.name.local == name)
        .map(|attribute| &*attribute.value)
}

/// Whether the element is hidden: the `hidden` attribute, or a style that
/// says `display: none` or `visibility: hidden`.
fn hidden(attributes: &[Attribute]) -> bool {
    attribute(attributes, "hidden").is_some()
        || attribute(attributes, "style").is_some_and(|style| {
            let style: String = style
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            style.contains("display:none") || style.contains("visibility:hidden")
        })
}

/// `html` as styled text. The markup is parsed as an engine parses it
/// (html5ever, the WHATWG algorithm), and the walk keeps its own stack, so
/// no nesting depth can exhaust ours.
#[must_use]
pub fn render(html: &str) -> Vec<Run> {
    let mut end = html.len().min(MAX_INPUT);
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    let dom = html5ever::parse_document(RcDom::default(), html5ever::ParseOpts::default())
        .one(html.get(..end).unwrap_or(""));
    let mut w = Writer::new();
    // The walk starts at the document's children and `dom` keeps the
    // document: dropping a node's last handle empties its whole subtree.
    let mut stack: Vec<Step> = dom
        .document
        .children
        .borrow()
        .iter()
        .rev()
        .cloned()
        .map(Step::Enter)
        .collect();
    while let Some(step) = stack.pop() {
        let node = match step {
            Step::Enter(node) => node,
            Step::Close(name) => {
                close_tag(&mut w, &name);
                continue;
            }
        };
        match &node.data {
            NodeData::Document => {}
            NodeData::Text { contents } => {
                w.text(&contents.borrow());
                continue;
            }
            NodeData::Element { name, attrs, .. } => {
                let attrs = attrs.borrow();
                // SVG and MathML are never shown, and go with their roots.
                if name.ns != ns!(html) || SKIPPED.contains(&&*name.local) || hidden(&attrs) {
                    continue;
                }
                open_tag(&mut w, &name.local, &attrs);
                stack.push(Step::Close(name.local.clone()));
            }
            NodeData::Doctype { .. }
            | NodeData::Comment { .. }
            | NodeData::ProcessingInstruction { .. } => continue,
        }
        stack.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(Step::Enter),
        );
    }
    // No trailing space or lines.
    while let Some(last) = w.runs.last_mut() {
        let trimmed = last.text.trim_end().len();
        last.text.truncate(trimmed);
        if last.text.is_empty() {
            w.runs.pop();
        } else {
            break;
        }
    }
    w.runs
}

fn open_tag(w: &mut Writer, name: &str, attributes: &[Attribute]) {
    if PARAGRAPH.contains(&name) {
        w.owe(2);
    } else if LINE.contains(&name) {
        w.owe(1);
    }
    match name {
        "b" | "strong" | "th" => w.bold += 1,
        "i" | "em" | "cite" | "dfn" | "var" => w.italic += 1,
        "h1" => w.heading.push(1),
        "h2" => w.heading.push(2),
        "h3" | "h4" | "h5" | "h6" => w.heading.push(3),
        "blockquote" => w.quote += 1,
        "pre" => w.pre += 1,
        "ul" => w.lists.push(None),
        "ol" => w.lists.push(Some(0)),
        "li" => w.item(),
        "hr" => {
            w.begin_text();
            w.push(Run {
                text: "────────".into(),
                ..w.style_plain()
            });
            w.owe(2);
        }
        "td" => {
            if !w.runs.is_empty() && w.owed == 0 {
                w.push(Run {
                    text: "  ".into(),
                    ..w.style_plain()
                });
                w.at_space = true;
            }
        }
        "a" => w
            .link
            .push(attribute(attributes, "href").and_then(safe_link)),
        "img" => {
            if let Some(alt) = attribute(attributes, "alt")
                .map(str::trim)
                .filter(|alt| !alt.is_empty())
            {
                w.text(alt);
            }
        }
        _ => {}
    }
}

fn close_tag(w: &mut Writer, name: &str) {
    if PARAGRAPH.contains(&name) {
        w.owe(2);
    } else if LINE.contains(&name) {
        w.owe(1);
    }
    match name {
        "b" | "strong" | "th" => w.bold = w.bold.saturating_sub(1),
        "i" | "em" | "cite" | "dfn" | "var" => w.italic = w.italic.saturating_sub(1),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            w.heading.pop();
        }
        "blockquote" => w.quote = w.quote.saturating_sub(1),
        "pre" => w.pre = w.pre.saturating_sub(1),
        "ul" | "ol" => {
            w.lists.pop();
        }
        "a" => {
            w.link.pop();
        }
        _ => {}
    }
}

/// A link target worth opening: the web and mail addresses only. A
/// `javascript:`, `file:` or `data:` link is plain text.
fn safe_link(href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        .then(|| href.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(html: &str) -> String {
        render(html).iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn paragraphs_lists_and_breaks_become_lines() {
        assert_eq!(
            text("<p>One</p><p>Two<br>three</p><ul><li>a</li><li>b</li></ul>"),
            "One\n\nTwo\nthree\n\n• a\n• b"
        );
        assert_eq!(text("<ol><li>x</li><li>y</li></ol>"), "1. x\n2. y");
    }

    #[test]
    fn whitespace_and_references_read_as_text() {
        assert_eq!(
            text("<div>  Fish &amp;\n   chips &#169; &#x41; &unknown; </div>"),
            "Fish & chips © A &unknown;"
        );
        assert_eq!(text("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2");
    }

    #[test]
    fn nothing_hidden_or_active_is_shown() {
        let shown = text(
            "<head><title>t</title><style>p{}</style></head><body>\
             <script>alert(1)</script><p>Hello</p>\
             <div style=\"display: none\">preheader <div>nested</div> junk</div>\
             <p hidden>secret</p><!-- note --><p>World</p>\
             <img src=\"https://tracker.example/p.gif\"><img src=x alt=\"Logo\"></body>",
        );
        assert_eq!(shown, "Hello\n\nWorld\n\nLogo");
        assert!(!shown.contains("tracker"));
    }

    #[test]
    fn styles_and_quotes_are_runs() {
        let runs = render(
            "<h1>Title</h1><p>A <b>bold</b> and <i>slanted</i> word</p><blockquote>quoted</blockquote>",
        );
        assert!(
            runs.iter()
                .any(|r| r.text == "Title" && r.heading == 1 && r.bold)
        );
        assert!(
            runs.iter()
                .any(|r| r.text == "bold" && r.bold && r.heading == 0)
        );
        assert!(runs.iter().any(|r| r.text == "slanted" && r.italic));
        let quoted: String = runs
            .iter()
            .filter(|r| r.quote)
            .map(|r| r.text.as_str())
            .collect();
        assert!(quoted.contains("> quoted"), "{quoted:?}");
    }

    #[test]
    fn only_web_and_mail_links_are_links() {
        let runs = render(
            "<a href=\"https://example.com/x?a=1&amp;b=2\">site</a> \
             <a href=\"JavaScript:alert(1)\">bad</a> \
             <a href=\"file:///etc/passwd\">file</a> \
             <a href=\"mailto:me@example.com\">mail</a>",
        );
        let link = |label: &str| {
            runs.iter()
                .find(|r| r.text == label)
                .and_then(|r| r.link.clone())
        };
        assert_eq!(
            link("site").as_deref(),
            Some("https://example.com/x?a=1&b=2")
        );
        assert_eq!(link("bad"), None);
        assert_eq!(link("file"), None);
        assert_eq!(link("mail").as_deref(), Some("mailto:me@example.com"));
    }

    #[test]
    fn hostile_markup_ends() {
        let nested = "<div>".repeat(50_000) + "x";
        assert_eq!(text(&nested), "x");
        // A tag the input ends inside is dropped, as an engine drops it.
        assert_eq!(text("before<a href=\"x"), "before");
        assert_eq!(text("<div style=\"display:none\">never closed"), "");
        assert_eq!(text("a<!-- never closed <b>b</b>"), "a");
        assert_eq!(text("a<script>never closed</p>b"), "a");
    }

    #[test]
    fn markup_is_read_as_an_engine_reads_it() {
        // Comments, CDATA outside foreign content, and processing
        // instructions are never text.
        assert_eq!(
            text("a<!-- <p>hidden</p> -->b<!--><p>c</p>-->"),
            "ab\n\nc\n\n-->"
        );
        assert_eq!(text("a<![CDATA[<b>x</b>]]>b"), "ax]]>b");
        assert_eq!(text("a<?php echo 1 ?>b"), "ab");
        // Script content ends at its end tag only, never inside a string.
        assert_eq!(
            text("<script>var s = \"<p>no</p>\";</script><p>yes</p>"),
            "yes"
        );
        // A tag name with a `<` in it is an unknown element, its text kept.
        assert_eq!(
            text("<scr<script>ipt>alert(1)</script>ok"),
            "ipt>alert(1)ok"
        );
        // Handlers and other attributes never show.
        assert_eq!(text("<p onclick=\"alert(1)\" title=\"t\">x</p>"), "x");
        // Misnested styles end where the engine ends them.
        let runs = render("<b>bold <i>both</b> slanted</i> plain");
        assert!(runs.iter().any(|r| r.text == "both" && r.bold && r.italic));
        assert!(
            runs.iter()
                .any(|r| r.text == " slanted" && !r.bold && r.italic)
        );
        assert!(
            runs.iter()
                .any(|r| r.text == " plain" && !r.bold && !r.italic)
        );
        // SVG and MathML show nothing, whatever they hold.
        assert_eq!(
            text("<svg><text>s</text></svg><math><mi>m</mi></math>x"),
            "x"
        );
        // Character references are the engine's whole table.
        assert_eq!(
            text("&hearts;&notin;&#x1F600;"),
            "\u{2665}\u{2209}\u{1F600}"
        );
    }
}
