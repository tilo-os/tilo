//! An HTML mail as a sealed document for the reader's web engine.
//!
//! A message's `text/html` part is hostile input: script, forms, frames, a meta refresh, and
//! remote images that report the mail was read. [`prepare`] parses the markup as an HTML
//! engine does (html5ever) and writes a new document holding only what a mail needs to look
//! like itself: known elements and attributes, web and mail links that open in a new window,
//! no remote content until the person allows it, and a Content-Security-Policy plus a request
//! filter that refuse every other connection. It loads as a `data:` URL, an opaque origin with
//! no cookies and no storage.

use std::fmt::Write as _;

use html5ever::tendril::TendrilSink as _;
use html5ever::{Attribute, LocalName, ns};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

const MAX_DATA_URL: usize = 2 * 1024 * 1024 - 1024;

const INLINE_BUDGET: usize = 1024 * 1024;

const MAX_LINKS: usize = 512;

const MAX_URL: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePart {
    pub id: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy<'a> {
    pub remote: bool,
    pub dark: bool,
    pub ground: &'a str,
    pub ink: &'a str,
    pub inline: &'a [InlinePart],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    pub document: String,
    pub links: Vec<String>,
    pub remote: usize,
    pub dark: bool,
}

pub fn prepare(html: &str, policy: &Policy<'_>) -> Prepared {
    let dark = policy.dark && dark_ready(html);
    let mut sanitizer = Sanitizer::new(policy, dark);
    sanitizer.run(html);
    let Sanitizer {
        out,
        links,
        remote,
        doctype,
        body,
        ..
    } = sanitizer;
    let mut document = String::with_capacity(out.len() + 1024);
    if let Some(doctype) = doctype {
        document.push_str(&doctype);
    }
    let csp = content_security_policy(policy.remote);
    let scheme = if dark { "dark" } else { "light" };
    let _ = write!(
        document,
        "<html><head>\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
<meta charset=\"utf-8\">\
<meta name=\"referrer\" content=\"no-referrer\">\
<meta name=\"color-scheme\" content=\"{scheme}\">\
<base target=\"_blank\">\
<style>{}</style></head><body{body}>",
        base_css(dark, policy)
    );
    document.push_str(&out);
    document.push_str("</body></html>");
    Prepared {
        document,
        links,
        remote,
        dark,
    }
}

pub fn links(html: &str) -> Vec<String> {
    let policy = Policy {
        remote: false,
        dark: false,
        ground: "#ffffff",
        ink: "#000000",
        inline: &[],
    };
    let mut sanitizer = Sanitizer::new(&policy, false);
    sanitizer.run(html);
    sanitizer.links
}

pub fn message_links(message: &crate::model::Message) -> Vec<String> {
    if let Some(html) = &message.html_body {
        return links(html);
    }
    let mut found: Vec<String> = Vec::new();
    for (_, url) in text_links(&message.body) {
        if found.len() < MAX_LINKS && !found.contains(&url) {
            found.push(url);
        }
    }
    found
}

pub fn text_links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() && found.len() < MAX_LINKS {
        let boundary = i == 0 || bytes.get(i - 1).is_some_and(|b| !b.is_ascii_alphanumeric());
        let rest = text.get(i..).unwrap_or_default();
        let web = boundary
            && (starts_ci(rest, "https://")
                || starts_ci(rest, "http://")
                || starts_ci(rest, "www."));
        if web {
            let end = i + url_extent(rest);
            let shown = text.get(i..end).unwrap_or_default();
            let valid = match shown.split_once("://") {
                Some((_, host)) => host.len() > 1,
                None => shown.get(4..).is_some_and(|host| host.contains('.')),
            };
            if valid {
                let url = if starts_ci(shown, "www.") {
                    format!("https://{shown}")
                } else {
                    shown.to_owned()
                };
                found.push((i..end, url));
                i = end;
                continue;
            }
        }
        if bytes.get(i) == Some(&b'@')
            && let Some((start, end)) = email_extent(text, i)
        {
            let address = text.get(start..end).unwrap_or_default();
            if found
                .last()
                .is_none_or(|(range, _): &(std::ops::Range<usize>, String)| range.end <= start)
            {
                found.push((start..end, format!("mailto:{address}")));
            }
            i = end;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    found
}

fn url_extent(text: &str) -> usize {
    let mut end = text
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '{' | '}'))
        .unwrap_or(text.len());
    loop {
        let shown = text.get(..end).unwrap_or_default();
        let Some(last) = shown.chars().last() else {
            return 0;
        };
        let unbalanced = last == ')' && shown.matches('(').count() < shown.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | ']') || unbalanced {
            end -= last.len_utf8();
        } else {
            return end;
        }
    }
}

fn email_extent(text: &str, at: usize) -> Option<(usize, usize)> {
    let local = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let host = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    let before = text.get(..at)?;
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| local(*c))
        .last()
        .map(|(index, _)| index)?;
    let after = text.get(at + 1..)?;
    let mut end = after.find(|c: char| !host(c)).unwrap_or(after.len());
    while after.get(..end).is_some_and(|h| h.ends_with(['.', '-'])) {
        end -= 1;
    }
    let domain = after.get(..end)?;
    let local_part = text.get(start..at)?;
    let valid = !local_part.starts_with('.')
        && domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && domain
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()));
    valid.then_some((start, at + 1 + end))
}

pub fn data_url(document: &str) -> Option<String> {
    use base64::Engine as _;
    const PREFIX: &str = "data:text/html;charset=utf-8;base64,";
    let encoded = document.len().div_ceil(3) * 4;
    if PREFIX.len() + encoded > MAX_DATA_URL {
        return None;
    }
    let mut url = String::with_capacity(PREFIX.len() + encoded);
    url.push_str(PREFIX);
    base64::engine::general_purpose::STANDARD.encode_string(document.as_bytes(), &mut url);
    Some(url)
}

pub fn content_security_policy(remote: bool) -> String {
    let web = if remote { " https: http:" } else { "" };
    format!(
        "default-src 'none'; script-src 'none'; object-src 'none'; frame-src 'none'; \
child-src 'none'; worker-src 'none'; connect-src 'none'; manifest-src 'none'; \
base-uri 'none'; form-action 'none'; style-src 'unsafe-inline' data:{web}; \
font-src data:{web}; img-src data:{web}; media-src data:{web}"
    )
}

pub fn filter_rules(remote: bool) -> String {
    const ALWAYS: &[&str] = &[
        "document",
        "subdocument",
        "script",
        "xmlhttprequest",
        "ping",
        "websocket",
        "object",
        "other",
    ];
    let mut rules = String::new();
    for scheme in ["http", "https"] {
        if !remote {
            let _ = writeln!(rules, "|{scheme}://");
            let _ = writeln!(rules, "|{scheme}://$all");
        }
        for kind in ALWAYS {
            let _ = writeln!(rules, "|{scheme}://${kind}");
        }
    }
    rules
}

fn dark_ready(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find("color-scheme")) {
        let at = from + found + "color-scheme".len();
        let window = lower
            .get(at..(at + 64).min(lower.len()))
            .unwrap_or_default();
        let end = window.find([';', '}', '>', ')']).unwrap_or(window.len());
        if window
            .get(..end)
            .is_some_and(|value| value.contains("dark"))
        {
            return true;
        }
        from = at;
    }
    false
}

pub const END_MARK: &str = "@layer{html::after{all:initial!important;content:\"\"!important;\
display:block!important;position:fixed!important;right:0!important;bottom:0!important;\
width:2px!important;height:2px!important;margin:0!important;\
background:rgb(1,1,1)!important;mix-blend-mode:difference!important;\
pointer-events:none!important;z-index:2147483647!important}\
html{display:block!important;visibility:visible!important;opacity:1!important;\
transform:none!important;translate:none!important;scale:none!important;\
rotate:none!important;perspective:none!important;filter:none!important;\
backdrop-filter:none!important;contain:none!important;\
container-type:normal!important;content-visibility:visible!important;\
will-change:auto!important;clip-path:none!important;mask:none!important;\
zoom:1!important;isolation:auto!important;mix-blend-mode:normal!important}}";

fn base_css(dark: bool, policy: &Policy<'_>) -> String {
    let ground = css_color(policy.ground);
    let ink = css_color(policy.ink);
    let mut css = if dark {
        format!(":root{{color-scheme:dark!important}}html{{background:{ground};color:{ink}}}")
    } else {
        ":root{color-scheme:light!important}html{background:#ffffff;color:#000000}".to_owned()
    };
    css.push_str(
        "body{overflow-wrap:break-word}pre{white-space:pre-wrap}\
img:not([height]){max-width:100%}",
    );
    // The view is as tall as the document and the reader scrolls it: the
    // page never shows a scroll bar of its own, not for the pixel a zoomed
    // mail rounds over its view.
    css.push_str("html{scrollbar-width:none!important}");
    css.push_str(END_MARK);
    css
}

fn css_color(color: &str) -> &str {
    let hex = color.strip_prefix('#').unwrap_or_default();
    if hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        color
    } else {
        "#000000"
    }
}

const ALLOWED: &[&str] = &[
    "a",
    "abbr",
    "acronym",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "fieldset",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

const VOID: &[&str] = &["br", "col", "hr", "img", "wbr"];

const DROPPED: &[&str] = &[
    "applet", "audio", "canvas", "dialog", "frameset", "iframe", "math", "noembed", "noframes",
    "noscript", "object", "portal", "script", "select", "svg", "template", "textarea", "title",
    "video", "xmp",
];

const PLAIN_ATTRIBUTES: &[&str] = &[
    "abbr",
    "align",
    "alink",
    "alt",
    "axis",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "char",
    "charoff",
    "class",
    "clear",
    "color",
    "cols",
    "colspan",
    "compact",
    "datetime",
    "dir",
    "face",
    "frame",
    "headers",
    "height",
    "hspace",
    "id",
    "lang",
    "leftmargin",
    "link",
    "marginheight",
    "marginwidth",
    "name",
    "noshade",
    "nowrap",
    "open",
    "reversed",
    "role",
    "rows",
    "rowspan",
    "rules",
    "scope",
    "size",
    "span",
    "start",
    "summary",
    "text",
    "title",
    "topmargin",
    "type",
    "valign",
    "value",
    "vlink",
    "vspace",
    "width",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Allowed,
    Body,
    Link,
    Style,
    Dropped,
    Unwrapped,
}

fn kind_of(name: &str) -> Kind {
    if ALLOWED.contains(&name) {
        Kind::Allowed
    } else if name == "body" {
        Kind::Body
    } else if name == "link" {
        Kind::Link
    } else if name == "style" {
        Kind::Style
    } else if DROPPED.contains(&name) {
        Kind::Dropped
    } else {
        Kind::Unwrapped
    }
}

fn attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.name.ns == ns!() && &*attribute.name.local == name)
        .map(|attribute| &*attribute.value)
}

fn doctype(name: &str, public: &str, system: &str) -> Option<String> {
    if !name.eq_ignore_ascii_case("html") || public.len() + system.len() > 512 {
        return None;
    }
    let quoted = |id: &str| -> String {
        id.chars()
            .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>'))
            .collect()
    };
    Some(match (public.is_empty(), system.is_empty()) {
        (true, true) => "<!DOCTYPE html>".to_owned(),
        (false, true) => format!("<!DOCTYPE html PUBLIC \"{}\">", quoted(public)),
        (true, false) => format!("<!DOCTYPE html SYSTEM \"{}\">", quoted(system)),
        (false, false) => format!(
            "<!DOCTYPE html PUBLIC \"{}\" \"{}\">",
            quoted(public),
            quoted(system)
        ),
    })
}

fn text_content(node: &Handle) -> String {
    let mut text = String::new();
    for child in node.children.borrow().iter() {
        if let NodeData::Text { contents } = &child.data {
            text.push_str(&contents.borrow());
        }
    }
    text
}

enum Step {
    Enter(Handle),
    Close(LocalName),
}

struct Sanitizer<'p> {
    policy: &'p Policy<'p>,
    dark: bool,
    out: String,
    links: Vec<String>,
    remote: usize,
    doctype: Option<String>,
    body: String,
    inline_budget: usize,
}

impl<'p> Sanitizer<'p> {
    fn new(policy: &'p Policy<'p>, dark: bool) -> Self {
        Self {
            policy,
            dark,
            out: String::new(),
            links: Vec::new(),
            remote: 0,
            doctype: None,
            body: String::new(),
            inline_budget: INLINE_BUDGET,
        }
    }

    fn run(&mut self, src: &str) {
        let dom =
            html5ever::parse_document(RcDom::default(), html5ever::ParseOpts::default()).one(src);
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
                    let _ = write!(self.out, "</{name}>");
                    continue;
                }
            };
            match &node.data {
                NodeData::Document => {}
                NodeData::Doctype {
                    name,
                    public_id,
                    system_id,
                } => {
                    self.doctype = self
                        .doctype
                        .take()
                        .or_else(|| doctype(name, public_id, system_id));
                    continue;
                }
                NodeData::Text { contents } => {
                    self.text(&contents.borrow());
                    continue;
                }
                NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => continue,
                NodeData::Element { name, attrs, .. } => {
                    // SVG and MathML go whole with their roots; nothing of
                    // another namespace is written.
                    if name.ns != ns!(html) {
                        continue;
                    }
                    let local = &*name.local;
                    let attrs = attrs.borrow();
                    match kind_of(local) {
                        Kind::Allowed => {
                            let attributes = self.attributes(local, &attrs);
                            let _ = write!(self.out, "<{local}{attributes}>");
                            if VOID.contains(&local) {
                                continue;
                            }
                            stack.push(Step::Close(name.local.clone()));
                        }
                        Kind::Body => self.body = self.attributes("body", &attrs),
                        Kind::Link => {
                            self.link(&attrs);
                            continue;
                        }
                        Kind::Style => {
                            let css = self.css(&text_content(&node), true);
                            self.out.push_str("<style>");
                            self.out.push_str(&css);
                            self.out.push_str("</style>");
                            continue;
                        }
                        Kind::Dropped => continue,
                        Kind::Unwrapped => {}
                    }
                }
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
    }

    fn text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '&' => self.out.push_str("&amp;"),
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                '\0' => {}
                _ => self.out.push(c),
            }
        }
    }

    fn link(&mut self, attributes: &[Attribute]) {
        let stylesheet = attribute(attributes, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        });
        if !stylesheet {
            return;
        }
        let Some(href) = attribute(attributes, "href").map(clean_url) else {
            return;
        };
        if let Some(url) = remote_url(&href) {
            self.remote += 1;
            if self.policy.remote {
                let _ = write!(
                    self.out,
                    "<link rel=\"stylesheet\" href=\"{}\">",
                    escape_decoded(&url)
                );
            }
        }
    }

    fn attributes(&mut self, element: &str, attributes: &[Attribute]) -> String {
        let mut out = String::new();
        let mut fragment = false;
        let mut shows = false;
        let mut style = String::new();
        for attribute in attributes {
            if attribute.name.ns != ns!() {
                continue;
            }
            let (name, value) = (&*attribute.name.local, &*attribute.value);
            match name {
                "href" if element == "a" => match link_target(&clean_url(value)) {
                    Some(Target::Fragment(at)) => {
                        fragment = true;
                        let _ = write!(out, " href=\"{}\"", escape_decoded(&at));
                    }
                    Some(Target::Address(url)) => {
                        let _ = write!(out, " href=\"{}\"", escape_decoded(&url));
                        if self.links.len() < MAX_LINKS && !self.links.contains(&url) {
                            self.links.push(url);
                        }
                    }
                    None => {}
                },
                "src" if element == "img" => {
                    if let Some(url) = self.image(&clean_url(value)) {
                        shows = true;
                        let _ = write!(out, " src=\"{}\"", escape_decoded(&url));
                    }
                }
                "srcset" if element == "img" => {
                    let kept = self.srcset(value);
                    if !kept.is_empty() {
                        shows = true;
                        let _ = write!(out, " srcset=\"{}\"", escape_decoded(&kept));
                    }
                }
                "background" => {
                    if let Some(url) = self.image(&clean_url(value)) {
                        let _ = write!(out, " background=\"{}\"", escape_decoded(&url));
                    }
                }
                "style" => {
                    style = self.css(value, false);
                }
                name if PLAIN_ATTRIBUTES.contains(&name) || is_aria(name) => {
                    let _ = write!(out, " {name}=\"{}\"", escape_decoded(value));
                }
                _ => {}
            }
        }
        if element == "img" && !shows {
            // An image held back is an empty box at the size the mail gives
            // it, and takes no room when it gives none: never the engine's
            // broken image and its alternative text over the layout. The
            // text stays its accessible name.
            let _ = write!(out, " src=\"{HELD_BACK_IMAGE}\"");
            let sized = attribute(attributes, "width").is_some()
                && attribute(attributes, "height").is_some()
                || ["width", "height"]
                    .iter()
                    .all(|side| css_declares(&style, side));
            if sized {
                style.push_str(";background-color:rgba(128,128,128,0.14)");
            }
            // Its proportions are the declared ones, not the held-back
            // pixel's square, when the mail scales it (`height:auto`).
            if let (Some(width), Some(height)) = (
                attribute(attributes, "width").and_then(image_length),
                attribute(attributes, "height").and_then(image_length),
            ) && !css_declares(&style, "aspect-ratio")
            {
                let _ = write!(style, ";aspect-ratio:{width}/{height}");
            }
        }
        if !style.trim().trim_matches(';').trim().is_empty() {
            let _ = write!(out, " style=\"{}\"", escape_decoded(&style));
        }
        if element == "a" {
            if fragment {
                out.push_str(" target=\"_self\"");
            } else {
                out.push_str(" target=\"_blank\" rel=\"noopener noreferrer\"");
            }
        }
        out
    }

    fn image(&mut self, url: &str) -> Option<String> {
        let scheme = scheme_of(url);
        match scheme.as_deref() {
            Some("cid") => self.inline_image(url.get(4..).unwrap_or_default()),
            Some("data") => data_image(url).then(|| url.to_owned()),
            _ => {
                let remote = remote_url(url)?;
                self.remote += 1;
                self.policy.remote.then_some(remote)
            }
        }
    }

    fn inline_image(&mut self, reference: &str) -> Option<String> {
        use base64::Engine as _;
        let id = percent_decode(reference);
        let id = id.trim().trim_start_matches('<').trim_end_matches('>');
        let part = self
            .policy
            .inline
            .iter()
            .find(|part| part.id.eq_ignore_ascii_case(id))?;
        let mime = part.mime.to_ascii_lowercase();
        if !image_mime(&mime) {
            return None;
        }
        let size = part.bytes.len().div_ceil(3) * 4 + mime.len() + 13;
        if size > self.inline_budget {
            return None;
        }
        self.inline_budget -= size;
        let mut url = format!("data:{mime};base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&part.bytes, &mut url);
        Some(url)
    }

    fn srcset(&mut self, value: &str) -> String {
        let mut kept: Vec<String> = Vec::new();
        let mut rest = value;
        loop {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
            if rest.is_empty() {
                break;
            }
            let end = rest
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(rest.len());
            let url = rest.get(..end).unwrap_or_default();
            let (url, ended) = match url.strip_suffix(',') {
                Some(trimmed) => (trimmed.trim_end_matches(','), true),
                None => (url, false),
            };
            rest = rest.get(end..).unwrap_or_default();
            let descriptor = if ended {
                ""
            } else {
                let stop = rest.find(',').unwrap_or(rest.len());
                let descriptor = rest.get(..stop).unwrap_or_default().trim();
                rest = rest.get(stop..).unwrap_or_default();
                descriptor
            };
            let descriptor_safe = descriptor
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == ' ');
            if let Some(url) = self.image(&clean_url(url))
                && descriptor_safe
            {
                kept.push(if descriptor.is_empty() {
                    url
                } else {
                    format!("{url} {descriptor}")
                });
            }
        }
        kept.join(", ")
    }

    fn css(&mut self, css: &str, sheet: bool) -> String {
        let css = plain_css_names(css);
        let mut out = String::with_capacity(css.len());
        let mut rest = css.as_str();
        while let Some(c) = rest.chars().next() {
            if rest.starts_with("/*") {
                let end = rest
                    .get(2..)
                    .and_then(|tail| tail.find("*/"))
                    .map_or(rest.len(), |at| at + 4);
                rest = rest.get(end..).unwrap_or_default();
                out.push(' ');
            } else if c == '"' || c == '\'' {
                let end = string_end(rest, c);
                let string = rest.get(..end).unwrap_or(rest);
                self.css_string_token(&mut out, string, c);
                rest = rest.get(end..).unwrap_or_default();
            } else if starts_ci(rest, "url(") {
                let (replacement, used) = self.css_url(rest);
                out.push_str(&replacement);
                rest = rest.get(used..).unwrap_or_default();
            } else if sheet && starts_ci(rest, "@import") {
                self.remote += 1;
                if self.policy.remote {
                    out.push_str("@import");
                    rest = rest.get(7..).unwrap_or_default();
                } else {
                    rest = rest.get(at_rule_end(rest)..).unwrap_or_default();
                    out.push(' ');
                }
            } else if starts_ci(rest, "prefers-color-scheme") {
                let (replacement, used) = self.color_scheme_query(rest);
                out.push_str(&replacement);
                rest = rest.get(used..).unwrap_or_default();
            } else if c == '<' {
                out.push_str("\\3c ");
                rest = rest.get(1..).unwrap_or_default();
            } else {
                out.push(c);
                rest = rest.get(c.len_utf8()..).unwrap_or_default();
            }
        }
        out
    }

    fn css_string_token(&mut self, out: &mut String, string: &str, quote: char) {
        let argument = out.trim_end().ends_with(['(', ',']);
        let inner = string
            .strip_prefix(quote)
            .map(|inner| inner.strip_suffix(quote).unwrap_or(inner))
            .unwrap_or_default();
        let url = clean_url(&css_unescape(inner));
        let fetched =
            argument && (remote_url(&url).is_some() || scheme_of(&url).as_deref() == Some("cid"));
        if fetched {
            let kept = self.image(&url).unwrap_or_default();
            let _ = write!(out, "\"{}\"", css_string(&kept));
        } else {
            push_css_text(out, string);
        }
    }

    fn css_url(&mut self, css: &str) -> (String, usize) {
        let open = 4;
        let inner = css.get(open..).unwrap_or_default();
        let trimmed_start = inner.len() - inner.trim_start().len();
        let body = inner.trim_start();
        let (raw, used) = if let Some(quote @ ('"' | '\'')) = body.chars().next() {
            let end = string_end(body, quote);
            let value = body.get(1..end.saturating_sub(1)).unwrap_or_default();
            let after = body.get(end..).unwrap_or_default();
            let close = after.find(')').map_or(after.len(), |at| at + 1);
            (value.to_owned(), open + trimmed_start + end + close)
        } else {
            let close = body.find(')').map_or(body.len(), |at| at + 1);
            let value = body.get(..close.saturating_sub(1)).unwrap_or(body).trim();
            (value.to_owned(), open + trimmed_start + close)
        };
        let url = clean_url(&css_unescape(&raw));
        let replacement = self
            .image(&url)
            .filter(|url| url.len() <= INLINE_BUDGET)
            .map_or_else(
                || "none".to_owned(),
                |url| format!("url(\"{}\")", css_string(&url)),
            );
        (replacement, used.min(css.len()))
    }

    fn color_scheme_query(&self, css: &str) -> (String, usize) {
        let name = "prefers-color-scheme";
        let after = css.get(name.len()..).unwrap_or_default();
        let value_start = after.len() - after.trim_start().len();
        let Some(rest) = after.trim_start().strip_prefix(':') else {
            return (name.to_owned(), name.len());
        };
        let value = rest.trim_start();
        let skipped = name.len() + value_start + 1 + (rest.len() - value.len());
        let word_len = value
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(value.len());
        let word = value
            .get(..word_len)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let holds = match word.as_str() {
            "dark" => self.dark,
            "light" => !self.dark,
            _ => return (name.to_owned(), name.len()),
        };
        let feature = if holds {
            "min-width:0px"
        } else {
            "max-width:-1px"
        };
        (feature.to_owned(), skipped + word_len)
    }
}

fn at_rule_end(css: &str) -> usize {
    let mut at = 0;
    let mut depth = 0_usize;
    while let Some(c) = css.get(at..).and_then(|tail| tail.chars().next()) {
        match c {
            '"' | '\'' => at += string_end(css.get(at..).unwrap_or_default(), c),
            '(' => {
                depth += 1;
                at += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            ';' if depth == 0 => return at + 1,
            _ => at += c.len_utf8(),
        }
    }
    css.len()
}

const HELD_BACK_IMAGE: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

fn image_length(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    raw.strip_suffix("px")
        .unwrap_or(raw)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|length| *length > 0)
}

fn css_declares(css: &str, name: &str) -> bool {
    css.split(';').any(|declaration| {
        declaration
            .split_once(':')
            .is_some_and(|(property, _)| property.trim().eq_ignore_ascii_case(name))
    })
}

fn starts_ci(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn plain_css_names(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.char_indices().peekable();
    let mut quote: Option<char> = None;
    while let Some((_, c)) = chars.next() {
        if let Some(open) = quote {
            out.push(c);
            if c == '\\' {
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            } else if c == open || c == '\n' {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
            continue;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(|(_, d)| d.is_ascii_hexdigit()) {
            hex.extend(chars.next().map(|(_, d)| d));
        }
        let decoded = if hex.is_empty() {
            chars.peek().map(|(_, d)| *d)
        } else {
            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
        };
        let name_char =
            decoded.filter(|d| d.is_ascii_alphanumeric() || matches!(d, '-' | '_' | '@'));
        match (name_char, hex.is_empty()) {
            (Some(d), true) => {
                chars.next();
                out.push(d);
            }
            (Some(d), false) => {
                if chars.peek().is_some_and(|(_, w)| w.is_ascii_whitespace()) {
                    chars.next();
                }
                out.push(d);
            }
            (None, _) => {
                out.push('\\');
                out.push_str(&hex);
            }
        }
    }
    out
}

fn string_end(css: &str, quote: char) -> usize {
    let mut escaped = false;
    for (at, c) in css.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote || c == '\n' {
            return at + c.len_utf8();
        }
    }
    css.len()
}

fn push_css_text(out: &mut String, text: &str) {
    for c in text.chars() {
        if c == '<' {
            out.push_str("\\3c ");
        } else {
            out.push(c);
        }
    }
}

fn css_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\22 "),
            '\\' => out.push_str("\\5c "),
            '<' => out.push_str("\\3c "),
            '\n' | '\r' | '\x0c' => {}
            _ => out.push(c),
        }
    }
    out
}

fn css_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
            hex.extend(chars.next());
        }
        if hex.is_empty() {
            out.extend(chars.next());
        } else {
            if chars.peek().is_some_and(char::is_ascii_whitespace) {
                chars.next();
            }
            out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
        }
    }
    out
}

enum Target {
    Fragment(String),
    Address(String),
}

fn link_target(url: &str) -> Option<Target> {
    if url.starts_with('#') {
        return Some(Target::Fragment(url.to_owned()));
    }
    match scheme_of(url).as_deref() {
        Some("http" | "https" | "mailto") => Some(Target::Address(url.to_owned())),
        None if url.starts_with("//") => Some(Target::Address(format!("https:{url}"))),
        _ => None,
    }
}

fn remote_url(url: &str) -> Option<String> {
    match scheme_of(url).as_deref() {
        Some("http" | "https") => Some(url.to_owned()),
        None if url.starts_with("//") => Some(format!("https:{url}")),
        _ => None,
    }
}

fn scheme_of(url: &str) -> Option<String> {
    let colon = url.find(':')?;
    let scheme = url.get(..colon)?;
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

fn data_image(url: &str) -> bool {
    let mime = url
        .get(5..)
        .unwrap_or_default()
        .split([';', ','])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    image_mime(&mime)
}

fn image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/gif"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "image/svg+xml"
    )
}

fn clean_url(url: &str) -> String {
    let cleaned: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c.is_control() || c == ' ');
    if trimmed.len() > MAX_URL {
        return String::new();
    }
    trimmed.to_owned()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        if let (b'%', Some(decoded)) = (byte, hex) {
            out.push(decoded);
            i += 3;
        } else {
            out.push(byte);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape_decoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\0' => {}
            c if c.is_control() => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn is_aria(name: &str) -> bool {
    name.strip_prefix("aria-").is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase() || c == '-')
    })
}

#[cfg(test)]
mod tests;
