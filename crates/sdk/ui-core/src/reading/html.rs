//! A tolerant HTML parser: markup as people and feeds actually write it,
//! read into a small element tree.
//!
//! It keeps what a reader needs: elements with their attributes, text with
//! its entities decoded, and the implied ends the HTML standard gives the
//! common elements (a `<p>` ends at the next block, an `<li>` at the next
//! item, a cell at the next cell), so `<p>one<p>two` and unclosed list items
//! read as the author meant. Scripts, styles and comments never reach the
//! tree. Nothing here fetches or runs anything.

use std::collections::HashMap;

/// The most markup read; the rest of a larger document is not shown.
pub const MAX_INPUT: usize = 8 * 1024 * 1024;
/// How deep elements nest before deeper ones are read as their parent's.
const MAX_DEPTH: usize = 256;

/// One node of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// The root.
    Document,
    Element {
        /// Lower case.
        name: String,
        /// Names lower case, values with entities decoded, in source order.
        attrs: Vec<(String, String)>,
    },
    Text(String),
}

#[derive(Clone, Debug)]
struct Slot {
    node: Node,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// A parsed document: an arena of nodes, the root at index 0.
#[derive(Clone, Debug)]
pub struct Dom {
    slots: Vec<Slot>,
}

/// Elements whose content is text, not markup. Only a title's is kept.
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "noscript", "noembed", "noframes", "iframe",
    "template",
];
/// Elements with no closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr", "keygen",
];
/// Start tags that end an open paragraph.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "dialog",
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
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];
/// Elements an implied end never reaches past.
const SCOPE: &[&str] = &[
    "html", "table", "td", "th", "caption", "marquee", "object", "template", "applet", "button",
];

impl Dom {
    /// Parse `html`. Never fails: whatever cannot be read as markup is
    /// text, and a document past [`MAX_INPUT`] is cut there.
    #[must_use]
    pub fn parse(html: &str) -> Self {
        let mut end = html.len().min(MAX_INPUT);
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        let mut builder = Builder::new();
        builder.run(html.get(..end).unwrap_or_default());
        builder.dom
    }

    /// The root's id.
    #[must_use]
    pub const fn root() -> usize {
        0
    }

    /// The node `id`.
    #[must_use]
    pub fn node(&self, id: usize) -> Option<&Node> {
        self.slots.get(id).map(|slot| &slot.node)
    }

    /// The children of `id`, in order.
    #[must_use]
    pub fn children(&self, id: usize) -> &[usize] {
        self.slots
            .get(id)
            .map_or(&[], |slot| slot.children.as_slice())
    }

    /// The parent of `id`.
    #[must_use]
    pub fn parent(&self, id: usize) -> Option<usize> {
        self.slots.get(id).and_then(|slot| slot.parent)
    }

    /// How many nodes the tree holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the tree holds only its root.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.len() <= 1
    }

    /// The element name of `id`, or `None` for text and the root.
    #[must_use]
    pub fn name(&self, id: usize) -> Option<&str> {
        match self.node(id)? {
            Node::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Whether `id` is an element named `name`.
    #[must_use]
    pub fn is(&self, id: usize, name: &str) -> bool {
        self.name(id) == Some(name)
    }

    /// The text of `id`, when it is a text node.
    #[must_use]
    pub fn text(&self, id: usize) -> Option<&str> {
        match self.node(id)? {
            Node::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The value of attribute `name` (lower case) on element `id`.
    #[must_use]
    pub fn attr(&self, id: usize, name: &str) -> Option<&str> {
        match self.node(id)? {
            Node::Element { attrs, .. } => attrs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str()),
            _ => None,
        }
    }

    /// Every descendant of `id` in document order, `id` excluded.
    #[must_use]
    pub fn descendants(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.children(id).iter().rev().copied().collect();
        while let Some(next) = stack.pop() {
            out.push(next);
            stack.extend(self.children(next).iter().rev().copied());
        }
        out
    }

    /// The first descendant of `id` named `name`, in document order.
    #[must_use]
    pub fn find(&self, id: usize, name: &str) -> Option<usize> {
        self.descendants(id)
            .into_iter()
            .find(|&node| self.is(node, name))
    }

    /// Every descendant of `id` named `name`, in document order.
    #[must_use]
    pub fn find_all(&self, id: usize, name: &str) -> Vec<usize> {
        self.descendants(id)
            .into_iter()
            .filter(|&node| self.is(node, name))
            .collect()
    }

    /// All the text under `id`, as written (whitespace kept).
    #[must_use]
    pub fn text_content(&self, id: usize) -> String {
        let mut out = String::new();
        if let Some(text) = self.text(id) {
            out.push_str(text);
        }
        for node in self.descendants(id) {
            if let Some(text) = self.text(node) {
                out.push_str(text);
            }
        }
        out
    }

    /// The text under `id` with its whitespace collapsed, trimmed.
    #[must_use]
    pub fn collapsed_text(&self, id: usize) -> String {
        collapse_whitespace(&self.text_content(id))
    }

    /// The ancestors of `id`, nearest first.
    #[must_use]
    pub fn ancestors(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = self.parent(id);
        while let Some(node) = at {
            out.push(node);
            at = self.parent(node);
        }
        out
    }

    /// Every element by its `id` attribute (the first, where ids repeat).
    #[must_use]
    pub fn by_id(&self) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for node in 0..self.slots.len() {
            if let Some(id) = self.attr(node, "id").filter(|id| !id.is_empty()) {
                map.entry(id.to_owned()).or_insert(node);
            }
        }
        map
    }
}

/// `text` with every run of whitespace one space, trimmed.
#[must_use]
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Builder {
    dom: Dom,
    /// The open elements, outermost first; the root is always at the bottom.
    stack: Vec<usize>,
}

impl Builder {
    fn new() -> Self {
        Self {
            dom: Dom {
                slots: vec![Slot {
                    node: Node::Document,
                    parent: None,
                    children: Vec::new(),
                }],
            },
            stack: vec![0],
        }
    }

    fn current(&self) -> usize {
        self.stack.last().copied().unwrap_or(0)
    }

    fn append(&mut self, node: Node) -> usize {
        let parent = self.current();
        let id = self.dom.slots.len();
        self.dom.slots.push(Slot {
            node,
            parent: Some(parent),
            children: Vec::new(),
        });
        if let Some(slot) = self.dom.slots.get_mut(parent) {
            slot.children.push(id);
        }
        id
    }

    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let parent = self.current();
        // Adjacent text joins its predecessor.
        if let Some(&last) = self
            .dom
            .slots
            .get(parent)
            .and_then(|slot| slot.children.last())
            && let Some(Slot {
                node: Node::Text(existing),
                ..
            }) = self.dom.slots.get_mut(last)
        {
            existing.push_str(text);
            return;
        }
        self.append(Node::Text(text.to_owned()));
    }

    fn open_name(&self, id: usize) -> &str {
        self.dom.name(id).unwrap_or_default()
    }

    /// The stack index of the nearest open `name`, not past a scope boundary
    /// or any of `also_stop`.
    fn in_scope(&self, name: &str, also_stop: &[&str]) -> Option<usize> {
        for (at, &id) in self.stack.iter().enumerate().rev() {
            let open = self.open_name(id);
            if open == name {
                return Some(at);
            }
            if SCOPE.contains(&open) || also_stop.contains(&open) {
                return None;
            }
        }
        None
    }

    fn pop_to(&mut self, at: usize) {
        self.stack.truncate(at.max(1));
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        let tag = name.as_str();
        if CLOSES_P.contains(&tag)
            && let Some(at) = self.in_scope("p", &[])
        {
            self.pop_to(at);
        }
        match tag {
            "li" => {
                if let Some(at) = self.in_scope("li", &["ul", "ol", "menu"]) {
                    self.pop_to(at);
                }
            }
            "dt" | "dd" => {
                let open = self
                    .in_scope("dt", &["dl"])
                    .into_iter()
                    .chain(self.in_scope("dd", &["dl"]))
                    .max();
                if let Some(at) = open {
                    self.pop_to(at);
                }
            }
            "tr" => {
                if let Some(at) = self.in_scope_table("tr") {
                    self.pop_to(at);
                }
            }
            "td" | "th" => {
                let open = self
                    .in_scope_table("td")
                    .into_iter()
                    .chain(self.in_scope_table("th"))
                    .max();
                if let Some(at) = open {
                    self.pop_to(at);
                }
            }
            "thead" | "tbody" | "tfoot" => {
                for section in ["thead", "tbody", "tfoot"] {
                    if let Some(at) = self.in_scope_table(section) {
                        self.pop_to(at);
                    }
                }
            }
            "option" => {
                if self.open_name(self.current()) == "option" {
                    self.stack.pop();
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if matches!(
                    self.open_name(self.current()),
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                ) {
                    self.stack.pop();
                }
            }
            _ => {}
        }
        let void = VOID.contains(&tag);
        let id = self.append(Node::Element { name, attrs });
        if !void && !self_closing && self.stack.len() < MAX_DEPTH {
            self.stack.push(id);
        }
    }

    /// The stack index of the nearest open `name` inside the nearest table.
    fn in_scope_table(&self, name: &str) -> Option<usize> {
        for (at, &id) in self.stack.iter().enumerate().rev() {
            let open = self.open_name(id);
            if open == name {
                return Some(at);
            }
            if matches!(open, "table" | "html") {
                return None;
            }
        }
        None
    }

    fn end(&mut self, name: &str) {
        // `</p>` and `</br>` with nothing open still mean something.
        if name == "br" {
            self.start("br".to_owned(), Vec::new(), true);
            return;
        }
        let mut target = None;
        for (at, &id) in self.stack.iter().enumerate().rev() {
            if at == 0 {
                break;
            }
            let open = self.open_name(id);
            if open == name {
                target = Some(at);
                break;
            }
            // An end tag never closes past a table boundary it is outside of.
            if SCOPE.contains(&open) && !SCOPE.contains(&name) {
                return;
            }
        }
        if let Some(at) = target {
            self.pop_to(at);
        }
    }

    fn run(&mut self, html: &str) {
        let mut rest = html;
        while !rest.is_empty() {
            let Some(open) = rest.find('<') else {
                self.text(&decode_entities(rest));
                break;
            };
            let (text, tail) = rest.split_at(open);
            if !text.is_empty() {
                self.text(&decode_entities(text));
            }
            rest = tail;
            if let Some(comment) = rest.strip_prefix("<!--") {
                rest = comment
                    .find("-->")
                    .map_or("", |end| after(comment, end + 3));
                continue;
            }
            if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
                let end = cdata.find("]]>").unwrap_or(cdata.len());
                self.text(cdata.get(..end).unwrap_or_default());
                rest = after(cdata, end + 3);
                continue;
            }
            // `<` starts a tag only before a name, a slash or a declaration.
            let tag_follows = rest
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
            let close = if tag_follows { tag_end(rest) } else { None };
            let Some(close) = close else {
                self.text("<");
                rest = after(rest, 1);
                continue;
            };
            let inner = rest.get(1..close).unwrap_or_default();
            rest = after(rest, close + 1);
            if inner.starts_with('!') || inner.starts_with('?') {
                continue;
            }
            let Some(tag) = Tag::parse(inner) else {
                continue;
            };
            if tag.closing {
                self.end(&tag.name);
                continue;
            }
            if RAW_TEXT.contains(&tag.name.as_str()) {
                let (content, tail) = raw_text(rest, &tag.name);
                rest = tail;
                if tag.name == "title" {
                    let name = tag.name.clone();
                    self.start(name, tag.attrs, false);
                    self.text(&decode_entities(content));
                    self.end("title");
                }
                continue;
            }
            self.start(tag.name, tag.attrs, tag.self_closing);
        }
    }
}

/// `s` from byte `n`; empty past the end.
fn after(s: &str, n: usize) -> &str {
    s.get(n..).unwrap_or("")
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

/// The content of the raw-text element `name` (a script, a style), whose
/// content is not markup, and what follows its end tag, whatever its case.
fn raw_text<'a>(rest: &'a str, name: &str) -> (&'a str, &'a str) {
    let needle = format!("</{name}");
    let bytes = rest.as_bytes();
    let found = bytes
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()));
    match found {
        Some(at) => {
            let tail = after(rest, at);
            (
                rest.get(..at).unwrap_or_default(),
                tail.find('>').map_or("", |end| after(tail, end + 1)),
            )
        }
        None => (rest, ""),
    }
}

struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn parse(inner: &str) -> Option<Self> {
        let inner = inner.trim();
        let (closing, inner) = inner
            .strip_prefix('/')
            .map_or((false, inner), |rest| (true, rest.trim_start()));
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let (name, attributes) = inner.split_at(name_end);
        let name = name.to_ascii_lowercase();
        // A namespaced name (`epub:switch`, `svg:svg`) is read by its local part.
        let local = name.rsplit_once(':').map(|(_, local)| local.to_owned());
        let name = local.unwrap_or(name);
        if !name.chars().next()?.is_ascii_alphabetic() {
            return None;
        }
        let attrs = if closing {
            Vec::new()
        } else {
            attributes_of(attributes)
        };
        Some(Self {
            name,
            closing,
            self_closing,
            attrs,
        })
    }
}

/// The attributes written in `s`: `name`, `name=value`, `name="value"`.
fn attributes_of(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut chars = s.char_indices().peekable();
    loop {
        while chars
            .next_if(|(_, c)| c.is_whitespace() || *c == '/')
            .is_some()
        {}
        let Some(&(start, _)) = chars.peek() else {
            break;
        };
        let mut end = start;
        while let Some(&(at, c)) = chars.peek() {
            if c.is_whitespace() || c == '=' || c == '/' {
                break;
            }
            end = at + c.len_utf8();
            chars.next();
        }
        let name = s.get(start..end).unwrap_or_default().to_ascii_lowercase();
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if(|(_, c)| *c == '=').is_some() {
            while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
            match chars.peek().map(|&(_, c)| c) {
                Some(quote @ ('"' | '\'')) => {
                    chars.next();
                    for (_, c) in chars.by_ref() {
                        if c == quote {
                            break;
                        }
                        value.push(c);
                    }
                }
                Some(_) => {
                    while let Some(&(_, c)) = chars.peek() {
                        if c.is_whitespace() {
                            break;
                        }
                        value.push(c);
                        chars.next();
                    }
                }
                None => {}
            }
        }
        if name.is_empty() {
            // A stray character that is no name: step past it.
            if chars.next().is_none() {
                break;
            }
            continue;
        }
        if !attrs.iter().any(|(key, _): &(String, String)| *key == name) {
            attrs.push((name, decode_entities(&value)));
        }
    }
    attrs
}

/// `text` with its character references (`&amp;`, `&#8217;`, `&#x2014;`)
/// decoded. A reference it does not know stays as written.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        rest = after(rest, amp);
        let body_end = rest
            .char_indices()
            .skip(1)
            .take(33)
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '#'))
            .map_or(rest.len().min(34), |(at, _)| at);
        let body = rest.get(1..body_end).unwrap_or_default();
        let terminated = rest
            .get(body_end..)
            .is_some_and(|tail| tail.starts_with(';'));
        match reference(body) {
            Some(decoded) if !body.is_empty() => {
                out.push_str(&decoded);
                rest = after(rest, body_end + usize::from(terminated));
            }
            _ => {
                out.push('&');
                rest = after(rest, 1);
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character(s) a reference body (without `&` and `;`) stands for.
fn reference(body: &str) -> Option<String> {
    if let Some(number) = body.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        // NUL and surrogates read as the replacement character; the C1
        // range is Windows-1252 as browsers read it.
        let code = match code {
            0x80 => 0x20AC,
            0x91 => 0x2018,
            0x92 => 0x2019,
            0x93 => 0x201C,
            0x94 => 0x201D,
            0x96 => 0x2013,
            0x97 => 0x2014,
            0x85 => 0x2026,
            other => other,
        };
        return Some(
            char::from_u32(code)
                .filter(|c| *c != '\0')
                .unwrap_or('\u{FFFD}')
                .to_string(),
        );
    }
    named(body).map(|c| c.to_string())
}

/// The named references people write, Latin-1 and typographic.
fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "hairsp" => '\u{200a}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "shy" => '\u{ad}',
        "hellip" | "mldr" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "minus" => '−',
        "lsquo" => '‘',
        "rsquo" | "rsquor" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" | "rdquor" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "lsaquo" => '‹',
        "rsaquo" => '›',
        "prime" => '′',
        "Prime" => '″',
        "bull" => '•',
        "middot" => '·',
        "dagger" => '†',
        "Dagger" => '‡',
        "permil" => '‰',
        "sect" => '§',
        "para" => '¶',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup1" => '¹',
        "sup2" => '²',
        "sup3" => '³',
        "micro" => 'µ',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "curren" => '¤',
        "iexcl" => '¡',
        "iquest" => '¿',
        "ordf" => 'ª',
        "ordm" => 'º',
        "not" => '¬',
        "macr" => '¯',
        "acute" => '´',
        "cedil" => '¸',
        "uml" => '¨',
        "brvbar" => '¦',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "harr" => '↔',
        "hookleftarrow" | "larrhk" => '↩',
        "check" | "checkmark" => '✓',
        "infin" => '∞',
        "ne" => '≠',
        "le" => '≤',
        "ge" => '≥',
        "asymp" => '≈',
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "pi" => 'π',
        "mu" => 'μ',
        "lambda" => 'λ',
        "sigma" => 'σ',
        "omega" => 'ω',
        "Agrave" => 'À',
        "Aacute" => 'Á',
        "Acirc" => 'Â',
        "Atilde" => 'Ã',
        "Auml" => 'Ä',
        "Aring" => 'Å',
        "AElig" => 'Æ',
        "Ccedil" => 'Ç',
        "Egrave" => 'È',
        "Eacute" => 'É',
        "Ecirc" => 'Ê',
        "Euml" => 'Ë',
        "Igrave" => 'Ì',
        "Iacute" => 'Í',
        "Icirc" => 'Î',
        "Iuml" => 'Ï',
        "ETH" => 'Ð',
        "Ntilde" => 'Ñ',
        "Ograve" => 'Ò',
        "Oacute" => 'Ó',
        "Ocirc" => 'Ô',
        "Otilde" => 'Õ',
        "Ouml" => 'Ö',
        "Oslash" => 'Ø',
        "Ugrave" => 'Ù',
        "Uacute" => 'Ú',
        "Ucirc" => 'Û',
        "Uuml" => 'Ü',
        "Yacute" => 'Ý',
        "THORN" => 'Þ',
        "szlig" => 'ß',
        "agrave" => 'à',
        "aacute" => 'á',
        "acirc" => 'â',
        "atilde" => 'ã',
        "auml" => 'ä',
        "aring" => 'å',
        "aelig" => 'æ',
        "ccedil" => 'ç',
        "egrave" => 'è',
        "eacute" => 'é',
        "ecirc" => 'ê',
        "euml" => 'ë',
        "igrave" => 'ì',
        "iacute" => 'í',
        "icirc" => 'î',
        "iuml" => 'ï',
        "eth" => 'ð',
        "ntilde" => 'ñ',
        "ograve" => 'ò',
        "oacute" => 'ó',
        "ocirc" => 'ô',
        "otilde" => 'õ',
        "ouml" => 'ö',
        "oslash" => 'ø',
        "ugrave" => 'ù',
        "uacute" => 'ú',
        "ucirc" => 'û',
        "uuml" => 'ü',
        "yacute" => 'ý',
        "thorn" => 'þ',
        "yuml" => 'ÿ',
        "OElig" => 'Œ',
        "oelig" => 'œ',
        "Scaron" => 'Š',
        "scaron" => 'š',
        "Yuml" => 'Ÿ',
        "fnof" => 'ƒ',
        _ => return None,
    })
}
