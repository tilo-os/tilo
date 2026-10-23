//! An HTML mail as a sealed document for the reader's web engine.
//!
//! A message's `text/html` part is hostile input. It can carry script,
//! forms that post what is typed into them, frames, plugins, a `<meta>`
//! refresh that navigates the moment the message opens, and remote images,
//! fonts and style sheets whose requests tell the sender the message was
//! read. [`prepare`] never hands the engine the mail's own markup: it reads
//! the markup into tokens and writes a new document from them that holds
//! only what a mail needs to look like itself.
//!
//! * **Only known elements and attributes survive.** Script, style sheets
//!   from elsewhere, frames, objects, embeds, SVG and `MathML`, templates,
//!   forms and their controls, `<base>`, `<meta>` and `<link>` are gone, and
//!   so is every event handler, `target`, `ping`, `srcdoc` and `formaction`.
//!   Unknown elements lose their tags and keep their text. Text is written
//!   escaped, attribute values quoted and escaped, so nothing in the mail can
//!   open a tag the sanitizer did not write.
//! * **Links are web and mail addresses only.** A link opens in a new
//!   window (`target="_blank"`), which the engine hands to Mail and Mail to
//!   the Browser; nothing navigates inside the message. Every link the
//!   document can open is listed in [`Prepared::links`].
//! * **Remote content is off until the person allows it.** Remote image
//!   sources, `srcset` candidates, `background` attributes, CSS `url()`s and
//!   `@import`s are removed and counted ([`Prepared::remote`]); allowed, they
//!   are kept and nothing else is. Inline parts (`cid:`) become `data:`
//!   images and always show, as do the mail's own `data:` images.
//! * **The document seals itself twice more.** A Content-Security-Policy is
//!   the first thing in its head ([`content_security_policy`]): no script,
//!   no frames, no objects, no form submission, no connections, and remote
//!   images, fonts and styles only when allowed. The engine's request filter
//!   ([`filter_rules`]) refuses every web request the same way before it
//!   leaves the process.
//! * **The mail's own colours stay.** A mail that says it has a dark form
//!   (a `color-scheme` naming `dark`, or a `prefers-color-scheme` query) is
//!   set dark in a dark reader, its dark rules on; every other mail is set
//!   light, on its own light sheet, never inverted. The queries are rewritten
//!   to what the reader chose, so the engine's own preference never decides.
//!
//! The document is loaded as a `data:` URL ([`data_url`]), an opaque origin
//! with no cookies and no storage.

use std::fmt::Write as _;

/// The longest `data:` URL the engine loads: Chromium refuses URLs over 2 MiB.
const MAX_DATA_URL: usize = 2 * 1024 * 1024 - 1024;

/// How much of the document inline images may take, base64 included, so the
/// document still fits a `data:` URL.
const INLINE_BUDGET: usize = 1024 * 1024;

/// The most links a document lists.
const MAX_LINKS: usize = 512;

/// The longest URL kept in an attribute.
const MAX_URL: usize = 8192;

/// An inline part of the message, named by its `Content-ID`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePart {
    /// The part's `Content-ID`, without angle brackets.
    pub id: String,
    /// Its MIME type, such as `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy<'a> {
    /// The person allowed remote content for this message.
    pub remote: bool,
    pub dark: bool,
    /// The reader's ground and ink as `#rrggbb`, for a mail set dark.
    pub ground: &'a str,
    pub ink: &'a str,
    /// The message's inline parts, for `cid:` references.
    pub inline: &'a [InlinePart],
}

/// A mail as a sealed document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// The whole document, to load as [`data_url`].
    pub document: String,
    /// Every address a link in it opens, in reading order, once each.
    pub links: Vec<String>,
    /// How many remote references the mail makes: removed unless allowed.
    pub remote: usize,
    /// The document is set dark: the mail has a dark form and the reader is
    /// dark. Otherwise it is a light sheet.
    pub dark: bool,
}

/// The mail `html` as a sealed document under `policy`.
pub fn prepare(html: &str, policy: &Policy<'_>) -> Prepared {
    let dark = policy.dark && dark_ready(html);
    let mut sanitizer = Sanitizer::new(policy, dark);
    sanitizer.run(html);
    let Sanitizer {
        out,
        links,
        remote,
        doctype,
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
<style>{}</style></head><body>",
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

/// Every web and mail address a link in `html` opens.
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

/// Every address a link of `message` opens: the links of its HTML, or
/// the addresses its text names.
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

/// The web and mail addresses in plain `text` and where each is:
/// `https://…` and `http://…`, `www.…` (opened over HTTPS) and `name@host`
/// (written to). Punctuation that ends a sentence is not part of one.
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
            // An address inside a web link was taken with it.
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

/// How far the web address starting `text` goes: to a space or a bracket
/// that cannot be in one, less the punctuation that ends a sentence, and a
/// closing parenthesis it did not open.
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

/// The mail address around the `@` at `at`: a local part before it and a
/// host with a dot after it.
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

/// The document as a `data:` URL, or `None` when it is too large to load as
/// one. Base64 keeps `#`, `%` and non-ASCII text intact.
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

/// The document's Content-Security-Policy. Script, objects, frames,
/// workers, connections, form submission and `<base>` are always refused;
/// images, fonts and styles come only from the document itself (`data:`)
/// unless remote content is allowed.
pub fn content_security_policy(remote: bool) -> String {
    let web = if remote { " https: http:" } else { "" };
    format!(
        "default-src 'none'; script-src 'none'; object-src 'none'; frame-src 'none'; \
child-src 'none'; worker-src 'none'; connect-src 'none'; manifest-src 'none'; \
base-uri 'none'; form-action 'none'; style-src 'unsafe-inline' data:{web}; \
font-src data:{web}; img-src data:{web}; media-src data:{web}"
    )
}

/// The engine's request rules (adblock syntax) for a mail document: every
/// web request refused, or with remote content allowed, everything but
/// images, fonts, style sheets and media. One type per rule, so a rule the
/// matcher does not know costs only itself.
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

/// Whether the mail says it has a dark form: a `color-scheme` (the meta tag
/// or the property, `supported-color-schemes` too) naming `dark`, or a
/// `prefers-color-scheme: dark` query.
fn dark_ready(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find("color-scheme")) {
        let at = from + found + "color-scheme".len();
        let window = lower
            .get(at..(at + 64).min(lower.len()))
            .unwrap_or_default();
        // Up to the end of the declaration, attribute or query.
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

/// The document's end mark: a square in the view's bottom-right corner,
/// blended to differ from what is under it by one level, so it is invisible
/// yet unlike the colour beside it. Offscreen Chromium on Linux paints a
/// resized view before the page has drawn at its new size: the page's old
/// frame, and the page's background colour in the rest. Only the page's own
/// frame at the size the view has carries the mark there, so the reader
/// shows a body once a frame has it (`reader::html`). A pseudo-element of the root,
/// above everything, so nothing the mail lays out moves or covers it.
///
/// The mail's style cannot remove it: the rules are important declarations in
/// the document's first cascade layer, which win over every important
/// declaration after it, layered or not, and the layer has no name for a
/// mail's rule to join. The mark resets every property it does not set, and
/// the root keeps nothing that would hide, move or contain it. The mail's own
/// root element, with its attributes, is never written.
/// `apps/browser/chromium/tests/resize_frames.py` holds Chromium to it.
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

/// The document's own style, before the mail's: a light sheet or the
/// reader's dark ground, preformatted text that wraps, long words that
/// break, and images without a set height that fit the width.
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

/// `color` if it is a `#rrggbb` colour, else black.
fn css_color(color: &str) -> &str {
    let hex = color.strip_prefix('#').unwrap_or_default();
    if hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        color
    } else {
        "#000000"
    }
}

/// Elements kept with their attributes.
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

/// Allowed elements without content or an end tag.
const VOID: &[&str] = &["br", "col", "hr", "img", "wbr"];

/// Elements removed with everything inside them.
const DROPPED: &[&str] = &[
    "applet", "audio", "canvas", "dialog", "frameset", "iframe", "math", "noembed", "noframes",
    "noscript", "object", "portal", "script", "select", "svg", "template", "textarea", "title",
    "video", "xmp",
];

/// Elements whose content is text up to their end tag, never markup.
const RAW_TEXT: &[&str] = &[
    "iframe", "noembed", "noframes", "noscript", "script", "style", "textarea", "title", "xmp",
];

/// Attributes kept as they are, on any allowed element.
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

/// What the sanitizer does with a start tag's element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Allowed,
    Body,
    Link,
    Dropped,
    /// Its tags go; its content stays.
    Unwrapped,
}

fn kind_of(name: &str) -> Kind {
    if ALLOWED.contains(&name) {
        Kind::Allowed
    } else if name == "body" {
        Kind::Body
    } else if name == "link" {
        Kind::Link
    } else if DROPPED.contains(&name) {
        Kind::Dropped
    } else {
        Kind::Unwrapped
    }
}

/// A start or end tag as read: its lower-case name, and its attributes with
/// lower-case names and raw values, the first of each name only.
#[derive(Debug, Default)]
struct Tag {
    name: String,
    attributes: Vec<(String, String)>,
    end: bool,
    self_closing: bool,
}

impl Tag {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Read the tag at `at` (just past its `<`). Returns it and where it ends,
/// or `None` when the input ends inside it (the tag is then dropped, as an
/// engine drops it).
fn read_tag(src: &str, at: usize) -> Option<(Tag, usize)> {
    let bytes = src.as_bytes();
    let mut i = at;
    let mut tag = Tag::default();
    if bytes.get(i) == Some(&b'/') {
        tag.end = true;
        i += 1;
    }
    let start = i;
    while let Some(&byte) = bytes.get(i) {
        if byte.is_ascii_whitespace() || byte == b'/' || byte == b'>' {
            break;
        }
        i += 1;
    }
    tag.name = src.get(start..i)?.to_ascii_lowercase();
    loop {
        // Between attributes.
        while let Some(&byte) = bytes.get(i) {
            if byte.is_ascii_whitespace() {
                i += 1;
            } else if byte == b'/' {
                tag.self_closing = bytes.get(i + 1) == Some(&b'>');
                i += 1;
            } else {
                break;
            }
        }
        if *bytes.get(i)? == b'>' {
            return Some((tag, i + 1));
        }
        tag.self_closing = false;
        let name_start = i;
        // The first character of a name may be `=`.
        i += 1;
        while let Some(&byte) = bytes.get(i) {
            if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'=') {
                break;
            }
            i += 1;
        }
        let name = src.get(name_start..i)?.to_ascii_lowercase();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let mut value = String::new();
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if let quote @ (b'"' | b'\'') = *bytes.get(i)? {
                let open = i + 1;
                let close = open + src.get(open..)?.find(char::from(quote))?;
                src.get(open..close)?.clone_into(&mut value);
                i = close + 1;
            } else {
                let open = i;
                while bytes
                    .get(i)
                    .is_some_and(|&byte| !byte.is_ascii_whitespace() && byte != b'>')
                {
                    i += 1;
                }
                src.get(open..i)?.clone_into(&mut value);
            }
        }
        if !tag.attributes.iter().any(|(key, _)| *key == name) {
            tag.attributes.push((name, value));
        }
    }
}

/// Where the raw text of element `name` that starts at `from` ends: the
/// start of its end tag, and just past it. Without one, the rest is text.
fn raw_text_end(src: &str, from: usize, name: &str) -> (usize, usize) {
    let lower = src.get(from..).unwrap_or_default().to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut search = 0;
    while let Some(found) = lower.get(search..).and_then(|rest| rest.find(&needle)) {
        let at = search + found;
        let after = at + needle.len();
        let next = lower.as_bytes().get(after).copied();
        if next.is_none_or(|byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'>') {
            let close = lower
                .get(after..)
                .and_then(|rest| rest.find('>'))
                .map_or(lower.len(), |gt| after + gt + 1);
            return (from + at, from + close);
        }
        search = after;
    }
    (src.len(), src.len())
}

struct Sanitizer<'p> {
    policy: &'p Policy<'p>,
    /// The document is set dark: `prefers-color-scheme: dark` holds.
    dark: bool,
    out: String,
    links: Vec<String>,
    remote: usize,
    /// The mail's doctype, when it has one: an HTML doctype keeps the mode
    /// the mail was written for; without one it renders in quirks mode, as
    /// in the mail clients it was written for.
    doctype: Option<String>,
    /// An element being dropped with its content, and how deep in nested
    /// elements of its name the reader is.
    skipping: Option<(String, usize)>,
    /// What inline images may still take of the document.
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
            skipping: None,
            inline_budget: INLINE_BUDGET,
        }
    }

    fn run(&mut self, src: &str) {
        let bytes = src.as_bytes();
        let mut i = 0;
        while i < src.len() {
            let Some(lt) = src
                .get(i..)
                .and_then(|rest| rest.find('<'))
                .map(|at| i + at)
            else {
                self.text(src.get(i..).unwrap_or_default());
                break;
            };
            self.text(src.get(i..lt).unwrap_or_default());
            match bytes.get(lt + 1).copied() {
                Some(b'!') => i = self.declaration(src, lt),
                Some(b'/') if bytes.get(lt + 2).is_some_and(u8::is_ascii_alphabetic) => {
                    let Some((tag, next)) = read_tag(src, lt + 1) else {
                        break;
                    };
                    self.end(&tag);
                    i = next;
                }
                Some(b'?' | b'/') => i = past(src, lt, ">"),
                Some(byte) if byte.is_ascii_alphabetic() => {
                    let Some((tag, next)) = read_tag(src, lt + 1) else {
                        break;
                    };
                    i = next;
                    if RAW_TEXT.contains(&tag.name.as_str()) {
                        let (content_end, close) = raw_text_end(src, next, &tag.name);
                        self.raw_element(&tag, src.get(next..content_end).unwrap_or_default());
                        i = close;
                    } else if tag.name == "plaintext" {
                        // The rest of the input is its text.
                        self.text(src.get(next..).unwrap_or_default());
                        break;
                    } else {
                        self.start(&tag);
                    }
                }
                _ => {
                    self.text("<");
                    i = lt + 1;
                }
            }
        }
    }

    /// A comment, doctype or other `<!` declaration at `lt`; returns where
    /// it ends. Only an HTML doctype before any content is kept.
    fn declaration(&mut self, src: &str, lt: usize) -> usize {
        let rest = src.get(lt..).unwrap_or_default();
        if rest.starts_with("<!--") {
            // `<!-->` and `<!--->` are empty comments.
            for empty in ["<!-->", "<!--->"] {
                if rest.starts_with(empty) {
                    return lt + empty.len();
                }
            }
            let body = lt + 4;
            let end = src.get(body..).and_then(|tail| {
                let close = tail.find("-->").map(|at| at + 3);
                let bang = tail.find("--!>").map(|at| at + 4);
                match (close, bang) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            });
            return end.map_or(src.len(), |end| body + end);
        }
        let end = past(src, lt, ">");
        let token = src.get(lt..end).unwrap_or_default();
        if self.doctype.is_none()
            && self.out.trim().is_empty()
            && token.len() <= 512
            && token
                .get(..14)
                .is_some_and(|head| head.eq_ignore_ascii_case("<!doctype html"))
            && token.ends_with('>')
            && !token.get(1..).unwrap_or_default().contains('<')
        {
            self.doctype = Some(token.chars().filter(|c| !c.is_control()).collect());
        }
        end
    }

    fn text(&mut self, text: &str) {
        if self.skipping.is_some() || text.is_empty() {
            return;
        }
        for c in text.chars() {
            match c {
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                '\0' => {}
                _ => self.out.push(c),
            }
        }
    }

    /// An element whose content is raw text: a style sheet is kept,
    /// rewritten; every other one goes with its text.
    fn raw_element(&mut self, tag: &Tag, content: &str) {
        if self.skipping.is_some() || tag.name != "style" {
            return;
        }
        let css = self.css(content, true);
        self.out.push_str("<style>");
        self.out.push_str(&css);
        self.out.push_str("</style>");
    }

    fn start(&mut self, tag: &Tag) {
        if let Some((name, depth)) = &mut self.skipping {
            if *name == tag.name && !tag.self_closing {
                *depth += 1;
            }
            return;
        }
        match kind_of(&tag.name) {
            Kind::Allowed => {
                let attributes = self.attributes(tag);
                let _ = write!(self.out, "<{}{attributes}>", tag.name);
            }
            Kind::Body => {
                let attributes = self.attributes(tag);
                let _ = write!(self.out, "<body{attributes}>");
            }
            Kind::Link => self.link(tag),
            Kind::Dropped => {
                // SVG and MathML close themselves with `/>`; HTML elements
                // stay open whatever their tag says.
                let foreign = matches!(tag.name.as_str(), "svg" | "math");
                if !(foreign && tag.self_closing) {
                    self.skipping = Some((tag.name.clone(), 1));
                }
            }
            Kind::Unwrapped => {}
        }
    }

    fn end(&mut self, tag: &Tag) {
        if let Some((name, depth)) = &mut self.skipping {
            if *name == tag.name {
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    self.skipping = None;
                }
            }
            return;
        }
        if kind_of(&tag.name) == Kind::Allowed && !VOID.contains(&tag.name.as_str()) {
            let _ = write!(self.out, "</{}>", tag.name);
        }
    }

    /// A `<link>`: a remote style sheet, kept only while remote content is
    /// allowed. Every other kind (prefetch, preconnect, DNS prefetch, icons)
    /// goes.
    fn link(&mut self, tag: &Tag) {
        let stylesheet = tag.attribute("rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        });
        if !stylesheet {
            return;
        }
        let Some(href) = tag.attribute("href").map(url_value) else {
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

    /// The attributes an allowed element keeps, written ` name="value"`.
    fn attributes(&mut self, tag: &Tag) -> String {
        let mut out = String::new();
        let mut fragment = false;
        let mut shows = false;
        let mut style = String::new();
        for (name, raw) in &tag.attributes {
            match name.as_str() {
                "href" if tag.name == "a" => match link_target(&url_value(raw)) {
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
                "src" if tag.name == "img" => {
                    if let Some(url) = self.image(&url_value(raw)) {
                        shows = true;
                        let _ = write!(out, " src=\"{}\"", escape_decoded(&url));
                    }
                }
                "srcset" if tag.name == "img" => {
                    let kept = self.srcset(raw);
                    if !kept.is_empty() {
                        shows = true;
                        let _ = write!(out, " srcset=\"{}\"", escape_decoded(&kept));
                    }
                }
                "background" => {
                    if let Some(url) = self.image(&url_value(raw)) {
                        let _ = write!(out, " background=\"{}\"", escape_decoded(&url));
                    }
                }
                "style" => {
                    style = self.css(&decode_entities(raw), false);
                }
                name if PLAIN_ATTRIBUTES.contains(&name) || is_aria(name) => {
                    let _ = write!(out, " {name}=\"{}\"", escape_attribute(raw));
                }
                _ => {}
            }
        }
        if tag.name == "img" && !shows {
            // An image held back is an empty box at the size the mail gives
            // it, and takes no room when it gives none: never the engine's
            // broken image and its alternative text over the layout. The
            // text stays its accessible name.
            let _ = write!(out, " src=\"{HELD_BACK_IMAGE}\"");
            let sized = tag.attribute("width").is_some() && tag.attribute("height").is_some()
                || ["width", "height"]
                    .iter()
                    .all(|side| css_declares(&style, side));
            if sized {
                style.push_str(";background-color:rgba(128,128,128,0.14)");
            }
            // Its proportions are the declared ones, not the held-back
            // pixel's square, when the mail scales it (`height:auto`).
            if let (Some(width), Some(height)) = (
                tag.attribute("width").and_then(image_length),
                tag.attribute("height").and_then(image_length),
            ) && !css_declares(&style, "aspect-ratio")
            {
                let _ = write!(style, ";aspect-ratio:{width}/{height}");
            }
        }
        if !style.trim().trim_matches(';').trim().is_empty() {
            let _ = write!(out, " style=\"{}\"", escape_decoded(&style));
        }
        if tag.name == "a" {
            // A link opens outside the message; one to a place in it stays.
            if fragment {
                out.push_str(" target=\"_self\"");
            } else {
                out.push_str(" target=\"_blank\" rel=\"noopener noreferrer\"");
            }
        }
        out
    }

    /// What an image source becomes: an inline part as a `data:` image, the
    /// mail's own `data:` image, a remote image while allowed; else nothing.
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

    /// The inline part a `cid:` reference names, as a `data:` image, while
    /// the document has room for it.
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

    /// The `srcset` candidates whose images may show.
    fn srcset(&mut self, raw: &str) -> String {
        let value = decode_entities(raw);
        let mut kept: Vec<String> = Vec::new();
        let mut rest = value.as_str();
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

    /// A style sheet or a `style` attribute rewritten: its `url()`s and
    /// `@import`s as [`Self::image`] allows them, the colour-scheme queries
    /// answered for the reader, and no `<` left in it.
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
                    // The whole rule goes, its address with it.
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

    /// A CSS string token. A string that is a function's argument may be
    /// fetched (`image-set("…")`, `@import "…"`): an address in one is an
    /// image as [`Self::image`] allows it, or the empty string.
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

    /// The `url(…)` at the start of `css`: what replaces it and how much of
    /// `css` it took.
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

    /// `prefers-color-scheme: dark` (or `light`) at the start of `css`,
    /// answered for the reader: a feature that always holds, or one that
    /// never does.
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

/// How far the statement at-rule at the start of `css` runs: through its
/// `;` outside strings and brackets, or to the end.
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

/// A transparent image one pixel square: what an image held back shows.
const HELD_BACK_IMAGE: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/// An image's `width` or `height` attribute as a length in pixels, if it
/// is one.
fn image_length(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    raw.strip_suffix("px")
        .unwrap_or(raw)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|length| *length > 0)
}

/// Whether the declarations in `css` set the property `name`.
fn css_declares(css: &str, name: &str) -> bool {
    css.split(';').any(|declaration| {
        declaration
            .split_once(':')
            .is_some_and(|(property, _)| property.trim().eq_ignore_ascii_case(name))
    })
}

/// Whether `text` starts with the ASCII `prefix`, in any case.
fn starts_ci(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// `css` with the escapes outside strings that spell a name character
/// resolved, so `u\72l(` reads as the `url(` the engine reads and
/// `@\69mport` as `@import`. Other escapes, and strings, stay as written.
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

/// Just past the first `needle` after `from`, or the end.
fn past(src: &str, from: usize, needle: &str) -> usize {
    src.get(from..)
        .and_then(|rest| rest.find(needle))
        .map_or(src.len(), |at| from + at + needle.len())
}

/// The end of the CSS string that starts `css` with `quote`, just past its
/// closing quote (or the end).
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

/// CSS text with `<` escaped.
fn push_css_text(out: &mut String, text: &str) {
    for c in text.chars() {
        if c == '<' {
            out.push_str("\\3c ");
        } else {
            out.push(c);
        }
    }
}

/// `value` as the inside of a double-quoted CSS string.
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

/// CSS escapes resolved: `\75 rl` is `url`, `\"` a quote.
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

/// What a link opens: a place in the message, or a web or mail address.
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

/// `url` as a remote address, protocol-relative ones on HTTPS; `None` for
/// any other kind.
fn remote_url(url: &str) -> Option<String> {
    match scheme_of(url).as_deref() {
        Some("http" | "https") => Some(url.to_owned()),
        None if url.starts_with("//") => Some(format!("https:{url}")),
        _ => None,
    }
}

/// The lower-case scheme of `url`, if it has one.
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

/// Whether `url` is a `data:` URL of an image kind a page shows.
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

/// An attribute's URL as the engine would read it: entities decoded,
/// whitespace and control characters gone.
fn url_value(raw: &str) -> String {
    clean_url(&decode_entities(raw))
}

/// `url` without tabs and newlines anywhere, or controls and spaces at its
/// ends, as URL parsers read it; bounded.
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

/// `%XX` escapes decoded.
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

/// Character references decoded: every numeric one, and the named ones a
/// URL can be spelled with. Other named references stay as written.
fn decode_entities(raw: &str) -> String {
    const NAMED: &[(&str, char)] = &[
        ("amp", '&'),
        ("lt", '<'),
        ("gt", '>'),
        ("quot", '"'),
        ("apos", '\''),
        ("nbsp", '\u{a0}'),
        ("colon", ':'),
        ("tab", '\t'),
        ("newline", '\n'),
        ("sol", '/'),
        ("period", '.'),
        ("comma", ','),
        ("lpar", '('),
        ("rpar", ')'),
        ("num", '#'),
        ("percnt", '%'),
        ("quest", '?'),
        ("equals", '='),
        ("plus", '+'),
        ("bsol", '\\'),
    ];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        if let Some(numeric) = after.strip_prefix('#') {
            let (radix, digits) = match numeric.strip_prefix(['x', 'X']) {
                Some(hex) => (16, hex),
                None => (10, numeric),
            };
            let len = digits
                .find(|c: char| !c.is_digit(radix))
                .unwrap_or(digits.len());
            if len > 0 {
                let value = digits
                    .get(..len.min(8))
                    .and_then(|d| u32::from_str_radix(d, radix).ok())
                    .and_then(char::from_u32)
                    .filter(|c| *c != '\0')
                    .unwrap_or('\u{fffd}');
                out.push(value);
                let mut used = 1 + (numeric.len() - digits.len()) + len;
                if digits.get(len..).is_some_and(|tail| tail.starts_with(';')) {
                    used += 1;
                }
                rest = after.get(used..).unwrap_or_default();
                continue;
            }
        } else if let Some(semi) = after.find(';').filter(|semi| *semi <= 10) {
            let name = after.get(..semi).unwrap_or_default();
            if let Some((_, c)) = NAMED
                .iter()
                .find(|(known, _)| name.eq_ignore_ascii_case(known))
            {
                out.push(*c);
                rest = after.get(semi + 1..).unwrap_or_default();
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// An attribute value, double-quoted: quotes and angle brackets escaped, an
/// ampersand kept where it starts a character reference, escaped elsewhere.
fn escape_attribute(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (at, c) in raw.char_indices() {
        match c {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\0' => {}
            '&' => {
                let tail = raw.get(at + 1..).unwrap_or_default();
                let len = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
                    .unwrap_or(tail.len());
                if len > 0 && tail.get(len..).is_some_and(|after| after.starts_with(';')) {
                    out.push('&');
                } else {
                    out.push_str("&amp;");
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A value already decoded (an address, a rewritten style) as an attribute
/// value: every `&` escaped, so nothing in it is decoded a second time, and
/// control characters as spaces.
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
