//! A small, tolerant HTML tree and the CSS selector subset the engine
//! scrapers and the page reader need.
//!
//! Result pages and arbitrary web pages are rarely valid markup, and none of
//! the callers here render anything: they pick a few elements out by selector
//! and read their text and attributes. So this builds a plain node tree the
//! way a forgiving reader would — unknown tags are kept, a stray end tag is
//! ignored, an end tag closes everything opened inside it, and the handful of
//! implied closes that change which element text belongs to (`<p>`, `<li>`,
//! table cells, `<option>`) are honoured. Scripts and styles are kept as raw
//! text and never read as markup. Nothing is fetched or run.
//!
//! The input is capped at [`MAX_INPUT`] bytes and the tree at [`MAX_NODES`]
//! nodes, so a hostile page costs a bounded amount of memory.

use std::fmt;

/// The most markup read; the rest of a larger page is ignored.
pub const MAX_INPUT: usize = 4 * 1024 * 1024;
/// The most nodes the tree holds; parsing stops there.
pub const MAX_NODES: usize = 200_000;
/// How deep elements nest before further opens are treated as siblings.
const MAX_DEPTH: usize = 256;

/// Elements that never have content or an end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Elements whose content is text that is never read as markup.
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title", "xmp", "noscript"];
/// Elements that close an open `<p>` when they start.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

/// Index of a node in its [`Document`].
pub type NodeId = usize;

#[derive(Debug, Clone)]
enum Kind {
    Root,
    Element {
        name: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A parsed page.
#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
}

impl Document {
    /// The root node: the parent of the top-level elements.
    pub const ROOT: NodeId = 0;

    /// Parse `html`, however malformed. Never fails.
    pub fn parse(html: &str) -> Self {
        let html = truncate_at_char(html, MAX_INPUT);
        let mut builder = Builder {
            doc: Document {
                nodes: vec![Node {
                    kind: Kind::Root,
                    parent: None,
                    children: Vec::new(),
                }],
            },
            stack: vec![Self::ROOT],
        };
        builder.run(html);
        builder.doc
    }

    /// The element's lower-case tag name, or `None` for text and the root.
    pub fn name(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// An attribute's value (entities decoded); `None` when it is absent.
    pub fn attr(&self, id: NodeId, attr: &str) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|(name, _)| name == attr)
                .map(|(_, value)| value.as_str()),
            _ => None,
        }
    }

    /// The node's text when it is a text node.
    pub fn text_node(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id)?.kind {
            Kind::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The node's children in document order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |n| n.children.as_slice())
    }

    /// The node's parent; `None` for the root.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    /// Every descendant of `id`, in document order, not including `id`.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        let mut stack: Vec<NodeId> = self.children(id).to_vec();
        stack.reverse();
        Descendants { doc: self, stack }
    }

    /// All text under `id`, as written (whitespace kept), skipping scripts and
    /// styles.
    pub fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.push_text(id, &mut out);
        out
    }

    fn push_text(&self, id: NodeId, out: &mut String) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        match &node.kind {
            Kind::Text(text) => out.push_str(text),
            Kind::Element { name, .. } if is_hidden_text(name) => {}
            _ => {
                for &child in &node.children {
                    self.push_text(child, out);
                }
            }
        }
    }

    /// The elements under `scope` that match `selector`, in document order.
    pub fn select(&self, scope: NodeId, selector: &Selector) -> Vec<NodeId> {
        self.descendants(scope)
            .filter(|&id| selector.matches(self, id))
            .collect()
    }

    /// The first element under `scope` that matches `selector`.
    pub fn select_first(&self, scope: NodeId, selector: &Selector) -> Option<NodeId> {
        self.descendants(scope)
            .find(|&id| selector.matches(self, id))
    }

    fn has_class(&self, id: NodeId, class: &str) -> bool {
        self.attr(id, "class")
            .is_some_and(|c| c.split_ascii_whitespace().any(|c| c == class))
    }

    fn element_siblings_before(&self, id: NodeId) -> bool {
        let Some(parent) = self.parent(id) else {
            return false;
        };
        self.children(parent)
            .iter()
            .take_while(|&&c| c != id)
            .any(|&c| self.name(c).is_some())
    }
}

/// Whether an element's text is never part of what a reader sees.
fn is_hidden_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "template" | "noscript")
}

/// Depth-first iterator over a subtree; see [`Document::descendants`].
pub struct Descendants<'a> {
    doc: &'a Document,
    stack: Vec<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.stack.pop()?;
        self.stack
            .extend(self.doc.children(id).iter().rev().copied());
        Some(id)
    }
}

struct Builder {
    doc: Document,
    stack: Vec<NodeId>,
}

impl Builder {
    fn current(&self) -> NodeId {
        self.stack.last().copied().unwrap_or(Document::ROOT)
    }

    fn full(&self) -> bool {
        self.doc.nodes.len() >= MAX_NODES
    }

    fn push_node(&mut self, kind: Kind) -> Option<NodeId> {
        if self.full() {
            return None;
        }
        let parent = self.current();
        let id = self.doc.nodes.len();
        self.doc.nodes.push(Node {
            kind,
            parent: Some(parent),
            children: Vec::new(),
        });
        if let Some(p) = self.doc.nodes.get_mut(parent) {
            p.children.push(id);
        }
        Some(id)
    }

    fn push_text(&mut self, raw: &str, decode: bool) {
        if raw.is_empty() {
            return;
        }
        let text = if decode {
            decode_entities(raw)
        } else {
            raw.to_owned()
        };
        // Adjacent text (split by a skipped comment) joins one node.
        let parent = self.current();
        if let Some(&last) = self.doc.nodes.get(parent).and_then(|p| p.children.last())
            && let Some(Node {
                kind: Kind::Text(existing),
                ..
            }) = self.doc.nodes.get_mut(last)
        {
            existing.push_str(&text);
            return;
        }
        self.push_node(Kind::Text(text));
    }

    fn open_name(&self, id: NodeId) -> Option<&str> {
        self.doc.name(id)
    }

    /// Pop up to and including the innermost open `name`, if it is open
    /// below any of `boundaries`.
    fn close(&mut self, name: &str, boundaries: &[&str]) -> bool {
        let mut found = None;
        for (depth, &id) in self.stack.iter().enumerate().rev() {
            match self.open_name(id) {
                Some(open) if open == name => {
                    found = Some(depth);
                    break;
                }
                Some(open) if boundaries.contains(&open) => break,
                _ => {}
            }
        }
        match found {
            Some(depth) if depth > 0 => {
                self.stack.truncate(depth);
                true
            }
            _ => false,
        }
    }

    fn implied_closes(&mut self, name: &str) {
        if CLOSES_P.contains(&name) {
            self.close("p", &["button", "table", "td", "th"]);
        }
        match name {
            "li" => {
                self.close("li", &["ul", "ol", "menu"]);
            }
            "dt" | "dd" => {
                if !self.close("dt", &["dl"]) {
                    self.close("dd", &["dl"]);
                }
            }
            "tr" => {
                self.close("tr", &["table", "tbody", "thead", "tfoot"]);
            }
            "td" | "th" => {
                if !self.close("td", &["tr", "table"]) {
                    self.close("th", &["tr", "table"]);
                }
            }
            "option" => {
                self.close("option", &["select", "datalist"]);
            }
            _ => {}
        }
    }

    /// Open an element; whether it was pushed as the new current element.
    fn start_tag(
        &mut self,
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    ) -> bool {
        self.implied_closes(&name);
        let void = VOID.contains(&name.as_str());
        let Some(id) = self.push_node(Kind::Element { name, attrs }) else {
            return false;
        };
        if !void && !self_closing && self.stack.len() < MAX_DEPTH {
            self.stack.push(id);
            return true;
        }
        false
    }

    fn end_tag(&mut self, name: &str) {
        self.close(name, &[]);
    }

    fn run(&mut self, html: &str) {
        let mut rest = html;
        while !rest.is_empty() && !self.full() {
            let Some(lt) = rest.find('<') else {
                self.push_text(rest, true);
                return;
            };
            let (text, from_lt) = rest.split_at(lt);
            self.push_text(text, true);
            rest = self.markup(from_lt);
        }
    }

    /// Read one piece of markup at `from_lt` (which starts with `<`) and
    /// return what follows it.
    fn markup<'a>(&mut self, from_lt: &'a str) -> &'a str {
        let after = from_lt.get(1..).unwrap_or_default();
        if let Some(comment) = after.strip_prefix("!--") {
            return comment.find("-->").map_or("", |end| skip(comment, end, 3));
        }
        if after.starts_with('!') || after.starts_with('?') {
            return after.find('>').map_or("", |end| skip(after, end, 1));
        }
        if let Some(end_tag) = after.strip_prefix('/') {
            let name_len = tag_name_len(end_tag);
            if name_len == 0 {
                // `</>` or `</ x`: not a tag; skip to its `>`.
                return end_tag.find('>').map_or("", |end| skip(end_tag, end, 1));
            }
            let name = end_tag
                .get(..name_len)
                .unwrap_or_default()
                .to_ascii_lowercase();
            self.end_tag(&name);
            return end_tag.find('>').map_or("", |end| skip(end_tag, end, 1));
        }
        let name_len = tag_name_len(after);
        if name_len == 0 {
            // A lone `<` in text.
            self.push_text("<", false);
            return after;
        }
        let (name, attrs_src) = after.split_at(name_len);
        let name = name.to_ascii_lowercase();
        let (attrs, self_closing, rest) = parse_attrs(attrs_src);
        if RAW_TEXT.contains(&name.as_str()) && !self_closing {
            let (body, rest) = raw_text_until_end(rest, &name);
            let decode = matches!(name.as_str(), "title" | "textarea");
            if self.start_tag(name, attrs, false) {
                self.push_text(body, decode);
                self.stack.pop();
            }
            return rest;
        }
        self.start_tag(name, attrs, self_closing);
        rest
    }
}

/// `s` after the `len`-byte match found at `at`.
fn skip(s: &str, at: usize, len: usize) -> &str {
    s.get(at.saturating_add(len)..).unwrap_or_default()
}

fn tag_name_len(s: &str) -> usize {
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, c)) if c.is_ascii_alphabetic() => {}
        _ => return 0,
    }
    chars
        .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '_'))
        .map_or(s.len(), |(i, _)| i)
}

/// The raw text of a script-like element and what follows its end tag.
fn raw_text_until_end<'a>(s: &'a str, name: &str) -> (&'a str, &'a str) {
    let closing = format!("</{name}");
    let mut search_from = 0;
    loop {
        let Some(found) = s
            .get(search_from..)
            .and_then(|tail| find_ascii_ci(tail, &closing))
        else {
            return (s, "");
        };
        let at = search_from.saturating_add(found);
        let after = s
            .get(at.saturating_add(closing.len())..)
            .unwrap_or_default();
        // `</scriptx` does not end a script.
        if after
            .chars()
            .next()
            .is_none_or(|c| c == '>' || c == '/' || c.is_ascii_whitespace())
        {
            let body = s.get(..at).unwrap_or_default();
            let rest = after.find('>').map_or("", |end| skip(after, end, 1));
            return (body, rest);
        }
        search_from = at.saturating_add(closing.len());
    }
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// Attributes of a start tag up to its `>`; whether it ended `/>`; the rest.
fn parse_attrs(mut s: &str) -> (Vec<(String, String)>, bool, &str) {
    let mut attrs: Vec<(String, String)> = Vec::new();
    loop {
        s = s.trim_start();
        let Some(c) = s.chars().next() else {
            return (attrs, false, "");
        };
        if c == '>' {
            return (attrs, false, s.get(1..).unwrap_or_default());
        }
        if let Some(after) = s.strip_prefix("/>") {
            return (attrs, true, after);
        }
        if c == '/' {
            s = s.get(1..).unwrap_or_default();
            continue;
        }
        let name_end = s
            .char_indices()
            .find(|&(_, c)| c.is_ascii_whitespace() || matches!(c, '=' | '>' | '/'))
            .map_or(s.len(), |(i, _)| i);
        let name_end = if name_end == 0 {
            // A stray `=` or quote; step over one character.
            c.len_utf8()
        } else {
            name_end
        };
        let (name, after_name) = s.split_at(name_end);
        let name = name.to_ascii_lowercase();
        let after_name_trim = after_name.trim_start();
        let (value, rest) = if let Some(v) = after_name_trim.strip_prefix('=') {
            let v = v.trim_start();
            if let Some(q @ ('"' | '\'')) = v.chars().next() {
                let body = v.get(1..).unwrap_or_default();
                match body.find(q) {
                    Some(end) => (body.get(..end).unwrap_or_default(), skip(body, end, 1)),
                    None => (body, ""),
                }
            } else {
                let end = v
                    .char_indices()
                    .find(|&(_, c)| c.is_ascii_whitespace() || c == '>')
                    .map_or(v.len(), |(i, _)| i);
                v.split_at(end)
            }
        } else {
            ("", after_name)
        };
        if !attrs.iter().any(|(n, _)| *n == name) {
            attrs.push((name, decode_entities(value)));
        }
        s = rest;
    }
}

/// Decode character references. Unknown named references stay as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        let (before, from_amp) = rest.split_at(amp);
        out.push_str(before);
        let after = from_amp.get(1..).unwrap_or_default();
        if let Some((c, consumed)) = decode_one(after) {
            out.push_str(&c);
            rest = after.get(consumed..).unwrap_or_default();
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// One reference after its `&`: the text it stands for and the bytes read.
fn decode_one(s: &str) -> Option<(String, usize)> {
    let end = s
        .char_indices()
        .take(12)
        .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '#'))
        .map_or_else(|| s.len().min(12), |(i, _)| i);
    let body = s.get(..end)?;
    let consumed = if s.get(end..)?.starts_with(';') {
        end.checked_add(1)?
    } else {
        end
    };
    if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        let c = char::from_u32(code)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}');
        return Some((c.to_string(), consumed));
    }
    let c = match body {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "ndash" => "\u{2013}",
        "mdash" => "\u{2014}",
        "hellip" => "\u{2026}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "middot" => "\u{b7}",
        "bull" => "\u{2022}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "times" => "\u{d7}",
        "deg" => "\u{b0}",
        "euro" => "\u{20ac}",
        "pound" => "\u{a3}",
        "zwj" => "\u{200d}",
        "zwnj" => "\u{200c}",
        "shy" => "\u{ad}",
        _ => return None,
    };
    // `&ampfoo` without a semicolon is read as `&` + `foo` only for the
    // legacy few; everything else needs the `;`.
    if consumed == end && !matches!(body, "amp" | "lt" | "gt" | "quot" | "nbsp") {
        return None;
    }
    Some((c.to_owned(), consumed))
}

fn truncate_at_char(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or_default()
}

/// A selector that did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError(String);

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported CSS selector: {}", self.0)
    }
}

impl std::error::Error for SelectorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttrOp {
    Exists,
    Equals(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, AttrOp)>,
    not: Vec<Compound>,
    first_child: bool,
}

impl Compound {
    fn matches(&self, doc: &Document, id: NodeId) -> bool {
        let Some(name) = doc.name(id) else {
            return false;
        };
        if self.tag.as_deref().is_some_and(|t| t != name) {
            return false;
        }
        if self
            .id
            .as_deref()
            .is_some_and(|want| doc.attr(id, "id") != Some(want))
        {
            return false;
        }
        if !self.classes.iter().all(|c| doc.has_class(id, c)) {
            return false;
        }
        let attrs_match = self.attrs.iter().all(|(attr, op)| match op {
            AttrOp::Exists => doc.attr(id, attr).is_some(),
            AttrOp::Equals(want) => doc.attr(id, attr) == Some(want.as_str()),
        });
        if !attrs_match {
            return false;
        }
        if self.first_child && doc.element_siblings_before(id) {
            return false;
        }
        !self.not.iter().any(|n| n.matches(doc, id))
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One selector chain, stored right to left: the subject first, then each
/// ancestor step with the combinator that led to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Complex {
    subject: Compound,
    ancestors: Vec<(Combinator, Compound)>,
}

impl Complex {
    fn matches(&self, doc: &Document, id: NodeId) -> bool {
        self.subject.matches(doc, id) && self.ancestors_match(doc, id, 0)
    }

    fn ancestors_match(&self, doc: &Document, id: NodeId, step: usize) -> bool {
        let Some((combinator, compound)) = self.ancestors.get(step) else {
            return true;
        };
        let next = step.saturating_add(1);
        let mut parent = doc.parent(id);
        while let Some(p) = parent {
            if compound.matches(doc, p) && self.ancestors_match(doc, p, next) {
                return true;
            }
            if *combinator == Combinator::Child {
                return false;
            }
            parent = doc.parent(p);
        }
        false
    }
}

/// A parsed CSS selector list.
///
/// Supported: type, `#id`, `.class`, `[attr]`, `[attr=value]` (quoted or
/// not), `:not(compound)`, `:first-child`, the descendant and child (`>`)
/// combinators, and `,` lists. That is what result-page scraping needs; a
/// selector outside it is an error rather than a silent mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector(Vec<Complex>);

impl Selector {
    /// Parse `css`.
    ///
    /// # Errors
    ///
    /// [`SelectorError`] for syntax outside the supported subset.
    pub fn parse(css: &str) -> Result<Self, SelectorError> {
        let mut list = Vec::new();
        for part in split_top_level(css, ',') {
            list.push(parse_complex(part.trim()).ok_or_else(|| SelectorError(css.to_owned()))?);
        }
        if list.is_empty() {
            return Err(SelectorError(css.to_owned()));
        }
        Ok(Self(list))
    }

    /// Whether the element `id` matches.
    pub fn matches(&self, doc: &Document, id: NodeId) -> bool {
        self.0.iter().any(|c| c.matches(doc, id))
    }
}

/// Split on `sep` outside brackets and parentheses.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth = depth.saturating_add(1),
            (None, ')' | ']') => depth = depth.saturating_sub(1),
            (None, c) if c == sep && depth == 0 => {
                parts.push(s.get(start..i).unwrap_or_default());
                start = i.saturating_add(c.len_utf8());
            }
            _ => {}
        }
    }
    parts.push(s.get(start..).unwrap_or_default());
    parts
}

fn parse_complex(s: &str) -> Option<Complex> {
    // Tokenise into compounds and combinators.
    let mut steps: Vec<(Combinator, Compound)> = Vec::new();
    let mut rest = s.trim();
    let mut pending = Combinator::Descendant;
    while !rest.is_empty() {
        let (compound, after) = parse_compound(rest)?;
        steps.push((pending, compound));
        let trimmed = after.trim_start();
        if let Some(child) = trimmed.strip_prefix('>') {
            pending = Combinator::Child;
            rest = child.trim_start();
            if rest.is_empty() {
                return None;
            }
        } else if trimmed.len() < after.len() || trimmed.is_empty() {
            pending = Combinator::Descendant;
            rest = trimmed;
        } else {
            return None;
        }
    }
    // Store right to left: each compound keeps the combinator that joins it
    // to the one on its right.
    let mut compounds = steps.into_iter().rev();
    let (mut joining, subject) = compounds.next()?;
    let mut ancestors = Vec::new();
    for (combinator, compound) in compounds {
        ancestors.push((joining, compound));
        joining = combinator;
    }
    Some(Complex { subject, ancestors })
}

fn ident_len(s: &str) -> usize {
    s.char_indices()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .map_or(s.len(), |(i, _)| i)
}

fn parse_compound(mut s: &str) -> Option<(Compound, &str)> {
    let mut compound = Compound::default();
    let universal = s.starts_with('*');
    if universal {
        s = s.get(1..)?;
    } else {
        let len = ident_len(s);
        if len > 0 {
            let (tag, rest) = s.split_at(len);
            compound.tag = Some(tag.to_ascii_lowercase());
            s = rest;
        }
    }
    loop {
        if let Some(rest) = s.strip_prefix('#') {
            let len = ident_len(rest);
            let (id, rest) = rest.split_at(len);
            if id.is_empty() {
                return None;
            }
            compound.id = Some(id.to_owned());
            s = rest;
        } else if let Some(rest) = s.strip_prefix('.') {
            let len = ident_len(rest);
            let (class, rest) = rest.split_at(len);
            if class.is_empty() {
                return None;
            }
            compound.classes.push(class.to_owned());
            s = rest;
        } else if let Some(rest) = s.strip_prefix('[') {
            let end = rest.find(']')?;
            let inner = rest.get(..end)?;
            s = skip(rest, end, 1);
            let attr = match inner.split_once('=') {
                Some((name, value)) => {
                    let value = value.trim();
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                        .unwrap_or(value);
                    (
                        name.trim().to_ascii_lowercase(),
                        AttrOp::Equals(value.to_owned()),
                    )
                }
                None => (inner.trim().to_ascii_lowercase(), AttrOp::Exists),
            };
            if attr.0.is_empty()
                || !attr
                    .0
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return None;
            }
            compound.attrs.push(attr);
        } else if let Some(rest) = s.strip_prefix(":not(") {
            let end = matching_paren(rest)?;
            let inner = rest.get(..end)?.trim();
            let (negated, leftover) = parse_compound(inner)?;
            if !leftover.trim().is_empty() || negated.is_empty() {
                return None;
            }
            compound.not.push(negated);
            s = skip(rest, end, 1);
        } else if let Some(rest) = s.strip_prefix(":first-child") {
            compound.first_child = true;
            s = rest;
        } else {
            break;
        }
    }
    if compound.is_empty() && !universal {
        return None;
    }
    Some((compound, s))
}

fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' if depth == 0 => return Some(i),
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// Collapse runs of whitespace (including non-breaking spaces) to one space
/// and trim the ends.
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s
        .split(|c: char| c.is_whitespace())
        .filter(|w| !w.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> Selector {
        Selector::parse(s).unwrap()
    }

    fn texts(doc: &Document, s: &str) -> Vec<String> {
        doc.select(Document::ROOT, &sel(s))
            .into_iter()
            .map(|id| collapse_whitespace(&doc.text(id)))
            .collect()
    }

    #[test]
    fn malformed_markup_still_nests_the_way_a_reader_expects() {
        let doc = Document::parse(
            "<ul><li>one<li>two <b>bold</ul><p>a<p>b</div><p>c &amp; d &copy; &#x41;&#66;",
        );
        assert_eq!(texts(&doc, "li"), ["one", "two bold"]);
        assert_eq!(texts(&doc, "p"), ["a", "b", "c & d \u{a9} AB"]);
        assert_eq!(texts(&doc, "ul > li > b"), ["bold"]);
    }

    #[test]
    fn scripts_and_comments_are_never_markup_or_text() {
        let doc = Document::parse(
            "<div id=x><!-- <p>no</p> --><script>if (a < b) { x = '</div>'; }</script>\
             <style>p{}</style>yes</div><p>after</p>",
        );
        assert_eq!(texts(&doc, "#x"), ["yes"]);
        assert_eq!(texts(&doc, "p"), ["after"]);
    }

    #[test]
    fn attributes_read_quoted_unquoted_and_bare() {
        let doc =
            Document::parse(r#"<a href="/x?a=1&amp;b=2" data-pos=3 hidden class='k  l'>t</a>"#);
        let a = doc.select_first(Document::ROOT, &sel("a")).unwrap();
        assert_eq!(doc.attr(a, "href"), Some("/x?a=1&b=2"));
        assert_eq!(doc.attr(a, "data-pos"), Some("3"));
        assert_eq!(doc.attr(a, "hidden"), Some(""));
        assert!(sel("a.k.l[data-pos='3'][hidden]").matches(&doc, a));
        assert!(!sel("a:not(.l)").matches(&doc, a));
    }

    #[test]
    fn combinators_and_lists_match_like_css() {
        let doc = Document::parse(
            "<main><div class=snippet data-pos=1><span class=title>A</span></div>\
             <div class='snippet standalone' data-pos=2><span class=title>B</span></div>\
             <section><div class=snippet data-pos=3><span class=title>C</span></div></section></main>\
             <div class=snippet data-pos=4><span class=title>D</span></div>",
        );
        assert_eq!(
            texts(&doc, "main .snippet[data-pos]:not(.standalone)"),
            ["A", "C"]
        );
        assert_eq!(texts(&doc, "main > .snippet .title"), ["A", "B"]);
        assert_eq!(
            texts(&doc, "section .title, main > div:first-child"),
            ["A", "C"]
        );
    }

    #[test]
    fn unsupported_selectors_are_errors() {
        for bad in ["", "a >", "a + b", "a:hover", "[x~=y]", ":not()"] {
            assert!(Selector::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn deep_and_huge_inputs_stay_bounded() {
        let deep = "<div>".repeat(10_000);
        let doc = Document::parse(&deep);
        assert!(doc.nodes.len() <= MAX_NODES);
        let many = "<i></i>".repeat(MAX_NODES.saturating_add(10));
        let doc = Document::parse(&many);
        assert_eq!(doc.nodes.len(), MAX_NODES);
    }
}
