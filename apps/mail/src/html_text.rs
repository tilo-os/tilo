//! An HTML mail as styled text.
//!
//! The reader shows a message's HTML part without an engine: the markup is
//! read into runs of text (bold, italic, headings, quotes, lists, links), and
//! nothing in it is ever fetched or run. Scripts, styles, hidden elements and
//! every attribute but a link's address are dropped, so there is no remote
//! content to block and no way for a message to tell the sender it was read.
//! Images show their alt text. Only web and mail addresses become links.

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

/// Elements whose content is text, not markup, and is never shown.
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title"];
/// Other elements whose content is never shown.
const SKIPPED: &[&str] = &[
    "head", "template", "noscript", "object", "iframe", "svg", "math", "select",
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
/// Elements with no closing tag.
const VOID: &[&str] = &[
    "br", "hr", "img", "meta", "link", "input", "area", "base", "col", "embed", "source", "track",
    "wbr",
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

/// `s` from byte `n`; empty past the end.
fn after(s: &str, n: usize) -> &str {
    s.get(n..).unwrap_or("")
}

#[must_use]
pub fn render(html: &str) -> Vec<Run> {
    let mut end = html.len().min(MAX_INPUT);
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    let mut rest = html.get(..end).unwrap_or("");
    let mut w = Writer::new();
    // The element being skipped, and how deeply it is nested in itself.
    let mut skipping: Option<(String, usize)> = None;
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            if skipping.is_none() {
                w.text(&decode(rest));
            }
            break;
        };
        let (text, tail) = rest.split_at(open);
        if !text.is_empty() && skipping.is_none() {
            w.text(&decode(text));
        }
        rest = tail;
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment
                .find("-->")
                .map_or("", |end| after(comment, end + 3));
            continue;
        }
        // `<` starts a tag only before a name, a slash or a declaration.
        let tag_follows = rest
            .chars()
            .nth(1)
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
        let close = if tag_follows { tag_end(rest) } else { None };
        let Some(close) = close else {
            if skipping.is_none() {
                w.text("<");
            }
            rest = after(rest, 1);
            continue;
        };
        let inner = rest.get(1..close).unwrap_or("");
        rest = after(rest, close + 1);
        let Some(tag) = Tag::parse(inner) else {
            continue;
        };
        if let Some((name, depth)) = skipping.as_mut() {
            if tag.name == *name && !VOID.contains(&tag.name.as_str()) {
                if tag.closing {
                    *depth -= 1;
                    if *depth == 0 {
                        skipping = None;
                    }
                } else {
                    *depth += 1;
                }
            }
            continue;
        }
        if tag.closing {
            close_tag(&mut w, &tag.name);
        } else if RAW_TEXT.contains(&tag.name.as_str()) {
            rest = after_raw_text(rest, &tag.name);
        } else if SKIPPED.contains(&tag.name.as_str()) || tag.hidden {
            if !VOID.contains(&tag.name.as_str()) {
                skipping = Some((tag.name, 1));
            }
        } else {
            open_tag(&mut w, &tag);
        }
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

/// Where the tag that opens `s` (which starts with `<`) ends, past any `>`
/// inside a quoted attribute.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// `rest` past the end tag of the raw-text element `name` (a script or a
/// style, whose content is not markup), whatever its case.
fn after_raw_text<'a>(rest: &'a str, name: &str) -> &'a str {
    let needle = format!("</{name}");
    let bytes = rest.as_bytes();
    let found = bytes
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()));
    match found {
        Some(at) => {
            let tail = after(rest, at);
            tail.find('>').map_or("", |end| after(tail, end + 1))
        }
        None => "",
    }
}

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
    hidden: bool,
}

impl Tag {
    fn parse(inner: &str) -> Option<Self> {
        let inner = inner.trim();
        let (closing, inner) = inner
            .strip_prefix('/')
            .map_or((false, inner), |rest| (true, rest));
        let inner = inner.trim_end_matches('/');
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let (name, attributes) = inner.split_at(name_end);
        let name = name.to_ascii_lowercase();
        if !name.chars().next()?.is_ascii_alphabetic() {
            return None;
        }
        let attrs = if closing {
            Vec::new()
        } else {
            attributes_of(attributes)
        };
        let hidden = attrs.iter().any(|(key, value)| {
            key == "hidden"
                || (key == "style" && {
                    let style: String = value
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                        .to_ascii_lowercase();
                    style.contains("display:none") || style.contains("visibility:hidden")
                })
        });
        Some(Self {
            name,
            closing,
            attrs,
            hidden,
        })
    }

    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

fn attributes_of(mut s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        let Some(key_end) = s.find(|c: char| c == '=' || c.is_whitespace()) else {
            if !s.is_empty() {
                out.push((s.to_ascii_lowercase(), String::new()));
            }
            return out;
        };
        let (key, tail) = s.split_at(key_end);
        let key = key.to_ascii_lowercase();
        s = tail.trim_start();
        let Some(value_start) = s.strip_prefix('=') else {
            out.push((key, String::new()));
            continue;
        };
        s = value_start.trim_start();
        let value;
        if let Some(quote) = s.chars().next().filter(|c| matches!(c, '"' | '\'')) {
            let body = after(s, 1);
            let end = body.find(quote).unwrap_or(body.len());
            let (text, tail) = body.split_at(end);
            value = text.to_owned();
            s = after(tail, 1);
        } else {
            let end = s.find(char::is_whitespace).unwrap_or(s.len());
            let (text, tail) = s.split_at(end);
            value = text.to_owned();
            s = tail;
        }
        out.push((key, decode(&value)));
    }
}

fn open_tag(w: &mut Writer, tag: &Tag) {
    let name = tag.name.as_str();
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
        "a" => w.link.push(tag.attr("href").and_then(safe_link)),
        "img" => {
            if let Some(alt) = tag.attr("alt").map(str::trim).filter(|alt| !alt.is_empty()) {
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

/// `text` with its character references replaced.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        let (before, tail) = rest.split_at(amp);
        out.push_str(before);
        let reference = after(tail, 1)
            .split_once(';')
            .map(|(name, _)| name)
            .filter(|name| name.len() <= 10);
        if let Some((name, c)) = reference.and_then(|name| entity(name).map(|c| (name, c))) {
            out.push(c);
            rest = after(tail, name.len() + 2);
        } else {
            out.push('&');
            rest = after(tail, 1);
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).filter(|c| *c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "ndash" => '–',
        "mdash" => '—',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "euro" => '€',
        "pound" => '£',
        "laquo" => '«',
        "raquo" => '»',
        "times" => '×',
        "deg" => '°',
        _ => return None,
    })
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
        assert_eq!(text("<a href=\"x"), "<a href=\"x");
        assert_eq!(text("<div style=\"display:none\">never closed"), "");
    }
}
