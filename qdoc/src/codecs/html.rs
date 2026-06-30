//! Built-in HTML **import** codec. The inverse of the `dochtml` export crate: it
//! parses an `.html` / `.htm` document back onto the canonical rich-text model so
//! the word processor can *open* HTML files.
//!
//! HTML in the wild is not strict XML (unclosed `<br>`/`<img>`/`<li>` tags, bare
//! `&` entities, attribute-quote variety), so this codec hand-rolls a **tolerant**
//! tag tokenizer rather than driving a strict XML parser: it never errors on
//! malformed markup, it degrades unknown tags to their text content, and it
//! unescapes the common HTML entities itself.
//!
//! ### Tag → model mapping (mirrors `dochtml`'s emit side)
//!
//! - `<h1>`…`<h6>`            → `heading` block carrying a `level` attr (1..=6).
//! - `<p>`                    → `paragraph` block.
//! - `<blockquote>`           → `quote` block (its inner `<p>`s flow into it).
//! - `<ul>` / `<ol>` `<li>`   → `list_item` blocks, `ordered` per `<ol>`, `level`
//!                              by list nesting depth (top-level lists are 0).
//! - `<table><tr><td|th>`     → `cell` blocks tagged `table` / `row` / `col`.
//! - `<pre>` / `<pre><code>`  → a block tagged `code`=true (content verbatim).
//! - `<img src>`              → an `image` block; a `data:` URI is decoded into a
//!                              content-addressed blob (`blob` attr = blake3 hex),
//!                              an `http(s)` src is kept on a `src` attr with the
//!                              `alt` text as the block text.
//!
//! ### Inline → run-mark mapping
//!
//! - `<strong>` / `<b>`       → `strong`            `<em>` / `<i>`   → `em`
//! - `<u>`                    → `underline`         `<s>`/`<del>`/`<strike>` → `strike`
//! - `<a href>`               → `link` mark (value = href).
//! - `<span style="color:…">` → `color` mark; `background[-color]:…` → `highlight`;
//!   `font-size:…` → `fontSize` (pt); `font-family:…` → `fontFamily` (generic).
//! - `<sup>` / `<sub>`        → `super` / `sub` boolean marks.
//! - `<code>` (inline)        → `fontFamily`=mono mark (mirrors the markdown codec).
//!
//! `<head>`, `<script>`, and `<style>` content is skipped wholesale (it is not
//! document body). HTML entities (`&amp;`, `&lt;`, `&#NN;`, `&#xHH;`, and the
//! common named ones) are unescaped in text and attribute values.
//!
//! `export` is intentionally `Unsupported`: HTML *export* lives in the editor via
//! the `dochtml` crate (it renders from a materialized `DocSnapshot`, which a
//! `FormatProvider` does not get). `qdoc` cannot depend on `dochtml` to reuse its
//! writer without a dependency cycle (`dochtml` depends on `qdoc`), so there is
//! exactly one HTML writer and it lives outside this crate.

use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};

pub struct HtmlCodec;

impl FormatProvider for HtmlCodec {
    fn schema_id(&self) -> &str {
        crate::schema::SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        // A leading `<!doctype html`, `<html`, or a `<` followed by a known
        // block tag is a strong HTML signal. Probe a generous prefix.
        let n = head.len().min(512);
        let s = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
        let t = s.trim_start();
        if t.starts_with("<!doctype html") || t.starts_with("<html") || t.starts_with("<head") {
            Confidence::High
        } else if t.starts_with("<h1") || t.starts_with("<p>") || t.starts_with("<body") || t.starts_with("<div") {
            Confidence::Medium
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        let src = String::from_utf8_lossy(bytes);
        import_html(&src, doc);
        Ok(())
    }

    fn export(&self, _doc: DocRef) -> Result<Vec<u8>, CodecError> {
        // Export deliberately lives in the editor via `dochtml::render_html`, which
        // renders from a materialized `DocSnapshot`. A `FormatProvider::export`
        // only gets a `DocRef`; reusing the `dochtml` writer here is impossible
        // anyway because `dochtml` depends on `qdoc` (a `qdoc → dochtml` dep would
        // be a cycle). We keep a single HTML writer and surface a clear error if
        // someone routes HTML export through the registry.
        Err(CodecError::Unsupported(
            "HTML export is provided by the `dochtml` crate (editor), not the qdoc codec".to_string(),
        ))
    }
}

// ---- tokenizer --------------------------------------------------------------

/// One lexical unit of an HTML stream.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A start tag: lowercased name + its raw attribute string (between the name
    /// and the closing `>` / `/>`).
    Open { name: String, attrs: String, self_closing: bool },
    /// An end tag `</name>`.
    Close { name: String },
    /// A run of literal (already entity-unescaped) text.
    Text(String),
}

/// Tokenize the HTML source into a flat tag/text stream. Tolerant: an unterminated
/// `<` is treated as literal text, and `<script>`/`<style>` bodies are consumed raw
/// (so their `<`-laden contents don't confuse the lexer) and then dropped.
fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out = Vec::new();
    let mut text = String::new();

    macro_rules! flush_text {
        () => {
            if !text.is_empty() {
                out.push(Token::Text(unescape_entities(&std::mem::take(&mut text))));
            }
        };
    }

    while i < n {
        let c = chars[i];
        if c == '<' {
            // A comment `<!-- … -->` — skip to its terminator.
            if starts_with_at(&chars, i, "<!--") {
                flush_text!();
                i += 4;
                while i < n && !starts_with_at(&chars, i, "-->") {
                    i += 1;
                }
                i = (i + 3).min(n);
                continue;
            }
            // A declaration/processing-instruction `<!doctype …>` / `<? … ?>`.
            if i + 1 < n && (chars[i + 1] == '!' || chars[i + 1] == '?') {
                flush_text!();
                while i < n && chars[i] != '>' {
                    i += 1;
                }
                i = (i + 1).min(n);
                continue;
            }
            // A tag must begin with `</` or `<letter`. Otherwise `<` is literal.
            let is_tag = i + 1 < n
                && (chars[i + 1] == '/' || chars[i + 1].is_ascii_alphabetic());
            if !is_tag {
                text.push(c);
                i += 1;
                continue;
            }
            // Find the matching `>` (HTML doesn't allow `>` inside an unquoted
            // attr; tolerate quoted `>` by tracking the active quote char).
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < n {
                let cj = chars[j];
                match quote {
                    Some(q) => {
                        if cj == q {
                            quote = None;
                        }
                    }
                    None => {
                        if cj == '"' || cj == '\'' {
                            quote = Some(cj);
                        } else if cj == '>' {
                            break;
                        }
                    }
                }
                j += 1;
            }
            if j >= n {
                // Unterminated tag → treat the rest as literal text.
                text.push(c);
                i += 1;
                continue;
            }
            let raw: String = chars[i + 1..j].iter().collect();
            flush_text!();
            if let Some(close) = raw.strip_prefix('/') {
                let name = close.trim().split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                if !name.is_empty() {
                    out.push(Token::Close { name });
                }
            } else {
                let self_closing = raw.trim_end().ends_with('/');
                let body = raw.trim_end().trim_end_matches('/');
                let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
                let name = body[..name_end].to_ascii_lowercase();
                let attrs = body[name_end..].trim().to_string();
                // `<script>` / `<style>` bodies are raw text (CDATA-like). Consume
                // to the matching close tag and drop both the tag and its content.
                if (name == "script" || name == "style") && !self_closing {
                    i = j + 1;
                    let close_tag = format!("</{name}");
                    while i < n && !starts_with_ci(&chars, i, &close_tag) {
                        i += 1;
                    }
                    // Skip past the close tag's `>`.
                    while i < n && chars[i] != '>' {
                        i += 1;
                    }
                    i = (i + 1).min(n);
                    continue;
                }
                out.push(Token::Open { name, attrs, self_closing });
            }
            i = j + 1;
        } else {
            text.push(c);
            i += 1;
        }
    }
    flush_text!();
    out
}

/// Whether `chars[i..]` starts with `pat` (ASCII, case-sensitive).
fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    if i + pc.len() > chars.len() {
        return false;
    }
    chars[i..i + pc.len()] == pc[..]
}

/// Whether `chars[i..]` starts with `pat`, case-insensitively (ASCII).
fn starts_with_ci(chars: &[char], i: usize, pat: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    if i + pc.len() > chars.len() {
        return false;
    }
    chars[i..i + pc.len()]
        .iter()
        .zip(pc.iter())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

// ---- entities ---------------------------------------------------------------

/// Unescape the common HTML entities in `s`: numeric (`&#NN;`, `&#xHH;`) and the
/// frequently-seen named ones. Unknown entities are passed through verbatim so no
/// data is lost. A bare `&` not starting a recognized entity stays literal.
fn unescape_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Scan for the terminating `;` within a small window.
        let mut j = i + 1;
        while j < n && j - i <= 12 && chars[j] != ';' && chars[j] != '&' && !chars[j].is_whitespace() {
            j += 1;
        }
        if j < n && chars[j] == ';' {
            let ent: String = chars[i + 1..j].iter().collect();
            if let Some(ch) = decode_entity(&ent) {
                out.push_str(&ch);
                i = j + 1;
                continue;
            }
        }
        // Not a recognized entity — keep the `&` literal.
        out.push('&');
        i += 1;
    }
    out
}

/// Decode a single entity body (the part between `&` and `;`) to its string, or
/// `None` if unrecognized.
fn decode_entity(ent: &str) -> Option<String> {
    if let Some(num) = ent.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let ch = match ent {
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
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "laquo" => '«',
        "raquo" => '»',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "middot" => '·',
        "bull" => '•',
        "dagger" => '†',
        "euro" => '€',
        "pound" => '£',
        "cent" => '¢',
        "yen" => '¥',
        "sect" => '§',
        "para" => '¶',
        "check" => '✓',
        _ => return None,
    };
    Some(ch.to_string())
}

// ---- attributes -------------------------------------------------------------

/// Parse a raw attribute string (`a="1" b='2' c=3 d`) into `(name_lower, value)`
/// pairs. Values are entity-unescaped; valueless attributes get an empty value.
fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out = Vec::new();
    while i < n {
        while i < n && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        if i >= n {
            break;
        }
        // Attribute name.
        let start = i;
        while i < n && !chars[i].is_whitespace() && chars[i] != '=' && chars[i] != '/' {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
        if name.is_empty() {
            i += 1;
            continue;
        }
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < n && chars[i] == '=' {
            i += 1;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            if i < n && (chars[i] == '"' || chars[i] == '\'') {
                let q = chars[i];
                i += 1;
                let vs = i;
                while i < n && chars[i] != q {
                    i += 1;
                }
                value = chars[vs..i].iter().collect();
                i = (i + 1).min(n);
            } else {
                let vs = i;
                while i < n && !chars[i].is_whitespace() {
                    i += 1;
                }
                value = chars[vs..i].iter().collect();
            }
        }
        out.push((name, unescape_entities(&value)));
    }
    out
}

/// The value of attribute `key` (case-insensitive) in a raw attr string, if present.
fn attr_value(raw: &str, key: &str) -> Option<String> {
    parse_attrs(raw).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Parse a `style="…"` declaration string into the model run-marks it implies.
/// Mirrors the markdown codec's `marks_from_span_style` so marks merge across
/// codecs (and back-convert what `dochtml` emits on its span styles).
fn marks_from_style(style: &str) -> Vec<(&'static str, MarkVal)> {
    let mut out: Vec<(&'static str, MarkVal)> = Vec::new();
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else { continue };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        match prop.as_str() {
            "color" => out.push(("color", MarkVal::Str(normalize_css_color(val)))),
            "background" | "background-color" => out.push(("highlight", MarkVal::Str(normalize_css_color(val)))),
            "font-size" => {
                let num = val.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%').trim();
                if let Ok(f) = num.parse::<f64>() {
                    out.push(("fontSize", MarkVal::Int(f.round() as i64)));
                }
            }
            "font-family" => out.push(("fontFamily", MarkVal::Str(generic_font_family(val).to_string()))),
            "font-weight" => {
                if val == "bold" || val.parse::<i32>().map(|w| w >= 600).unwrap_or(false) {
                    out.push(("strong", MarkVal::Bool(true)));
                }
            }
            "font-style" => {
                if val == "italic" || val == "oblique" {
                    out.push(("em", MarkVal::Bool(true)));
                }
            }
            "text-decoration" | "text-decoration-line" => {
                if val.contains("underline") {
                    out.push(("underline", MarkVal::Bool(true)));
                }
                if val.contains("line-through") {
                    out.push(("strike", MarkVal::Bool(true)));
                }
            }
            _ => {}
        }
    }
    out
}

/// Normalise a CSS colour value to the model's canonical `#rrggbb` (lower-case
/// hex; a handful of CSS keywords; otherwise passed through trimmed).
fn normalize_css_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return format!("#{}", hex.to_ascii_lowercase());
        }
        if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            // Expand `#abc` → `#aabbcc`.
            let mut full = String::from("#");
            for c in hex.chars() {
                full.push(c.to_ascii_lowercase());
                full.push(c.to_ascii_lowercase());
            }
            return full;
        }
    }
    match t.to_ascii_lowercase().as_str() {
        "red" => "#ff0000".into(),
        "green" => "#008000".into(),
        "blue" => "#0000ff".into(),
        "black" => "#000000".into(),
        "white" => "#ffffff".into(),
        "yellow" => "#ffff00".into(),
        _ => t.to_string(),
    }
}

/// Collapse a CSS `font-family` value onto one of the model's generic families.
fn generic_font_family(val: &str) -> &'static str {
    let v = val.to_ascii_lowercase();
    if v.contains("mono") || v.contains("courier") || v.contains("consol") {
        "mono"
    } else if v.contains("times") || v.contains("georgia") || v.contains("garamond") {
        "serif"
    } else if v.contains("serif") && !v.contains("sans") {
        "serif"
    } else {
        "sans"
    }
}

// ---- import state machine ---------------------------------------------------

/// A mark value: boolean toggle, a string payload, or an integer (fontSize).
#[derive(Clone)]
enum MarkVal {
    Bool(bool),
    Str(String),
    Int(i64),
}

/// An open inline wrapper on the stack: the marks it contributes + the char index
/// in the current block where it opened. Closed (and applied) on its end tag.
struct OpenInline {
    /// The HTML tag name that opened this wrapper (for matched-close lookup).
    tag: String,
    /// `(mark_name, value, start_char)` triples to apply when the wrapper closes.
    marks: Vec<(&'static str, MarkVal)>,
    start: usize,
}

/// A run mark gathered during parsing, applied after the block's text is complete.
struct PendingMark {
    block: String,
    start: usize,
    end: usize,
    name: &'static str,
    value: MarkVal,
}

/// One open list context: whether it is ordered, tracked so nested `<li>`s carry
/// the right `ordered` + `level`.
struct ListCtx {
    ordered: bool,
}

fn import_html(src: &str, doc: &mut DocMut) {
    let tokens = tokenize(src);

    // Block-building cursor.
    let mut prev_block: Option<String> = None;
    let mut cur_block: Option<String> = None;
    let mut cur_len: usize = 0;

    // Inline-wrapper stack and the marks gathered so far.
    let mut inline: Vec<OpenInline> = Vec::new();
    let mut pending: Vec<PendingMark> = Vec::new();

    // List nesting; `<li>`s use the depth for `level` and the top ctx for `ordered`.
    let mut lists: Vec<ListCtx> = Vec::new();

    // Table state: cells become `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;
    // Whether we saw any `<tr>` yet in the current table (a leading `<td>` without
    // a `<tr>` still gets row 0).
    let mut in_row = false;

    // Skip-content depth: inside `<head>`/`<title>` etc. we drop all text until the
    // matching close. Tracked by the tag name we're skipping until.
    let mut skip_until: Option<String> = None;

    // Quote nesting: while inside a `<blockquote>`, a bare `<p>` flows into the
    // quote block rather than opening a fresh paragraph. Depth > 0 = inside a quote.
    let mut quote_depth: usize = 0;
    // Code block depth: while inside `<pre>`, text is verbatim and `<code>` does not
    // add an inline mono mark.
    let mut pre_depth: usize = 0;

    for tok in tokens {
        // While skipping a non-body region, only watch for its closing tag.
        if let Some(skip) = &skip_until {
            if let Token::Close { name } = &tok {
                if name == skip {
                    skip_until = None;
                }
            }
            continue;
        }

        match tok {
            Token::Open { name, attrs, self_closing } => {
                match name.as_str() {
                    // ---- non-body regions to skip wholesale ------------------
                    "head" | "title" => {
                        if !self_closing {
                            skip_until = Some(name.clone());
                        }
                    }
                    // Structural containers we transparently pass through.
                    "html" | "body" | "div" | "section" | "article" | "main" | "header"
                    | "footer" | "nav" | "figure" | "figcaption" | "tbody" | "thead" | "tfoot"
                    | "colgroup" | "col" => {}
                    // ---- headings -------------------------------------------
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        let level = name[1..].parse::<i64>().unwrap_or(1).clamp(1, 6);
                        let id = open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "heading");
                        doc.set_attr(&id, "level", level);
                    }
                    "p" => {
                        if quote_depth > 0 {
                            // Inside a blockquote, the `<p>` text flows into the
                            // quote block (open it lazily if needed).
                            if cur_block.is_none() {
                                let id = open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "quote");
                                let _ = id;
                            }
                        } else {
                            finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                            open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "paragraph");
                        }
                    }
                    "blockquote" => {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        // Open the quote block now; nested `<p>`s flow into it.
                        open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "quote");
                        quote_depth += 1;
                    }
                    "br" => {
                        if let Some(b) = cur_block.as_ref() {
                            doc.splice_text(b, cur_len, 0, "\n");
                            cur_len += 1;
                        }
                    }
                    "hr" => { /* no modeled construct; ignore */ }
                    // ---- lists ----------------------------------------------
                    "ul" => lists.push(ListCtx { ordered: false }),
                    "ol" => lists.push(ListCtx { ordered: true }),
                    "li" => {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        let ordered = lists.last().map(|l| l.ordered).unwrap_or(false);
                        let level = lists.len().saturating_sub(1);
                        let id = open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "list_item");
                        doc.set_attr(&id, "ordered", ordered);
                        if level > 0 {
                            doc.set_attr(&id, "level", level as i64);
                        }
                    }
                    // ---- tables ---------------------------------------------
                    "table" => {
                        table_seq += 1;
                        table_id = Some(format!("t{table_seq}"));
                        trow = 0;
                        in_row = false;
                    }
                    "tr" => {
                        if in_row {
                            trow += 1;
                        }
                        in_row = true;
                        tcol = 0;
                    }
                    "td" | "th" => {
                        if let Some(tid) = table_id.clone() {
                            finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                            let id = open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "cell");
                            doc.set_attr(&id, "table", tid.as_str());
                            doc.set_attr(&id, "row", trow as i64);
                            doc.set_attr(&id, "col", tcol as i64);
                        }
                    }
                    // ---- code -----------------------------------------------
                    "pre" => {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        let id = open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "paragraph");
                        doc.set_attr(&id, "code", true);
                        pre_depth += 1;
                    }
                    // ---- images ---------------------------------------------
                    "img" => {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        insert_image(doc, &mut prev_block, &mut cur_block, &mut cur_len, &attrs);
                    }
                    // ---- inline wrappers ------------------------------------
                    "strong" | "b" => push_inline(&mut inline, &name, cur_len, vec![("strong", MarkVal::Bool(true))]),
                    "em" | "i" => push_inline(&mut inline, &name, cur_len, vec![("em", MarkVal::Bool(true))]),
                    "u" | "ins" => push_inline(&mut inline, &name, cur_len, vec![("underline", MarkVal::Bool(true))]),
                    "s" | "strike" | "del" => push_inline(&mut inline, &name, cur_len, vec![("strike", MarkVal::Bool(true))]),
                    "sup" => push_inline(&mut inline, &name, cur_len, vec![("super", MarkVal::Bool(true))]),
                    "sub" => push_inline(&mut inline, &name, cur_len, vec![("sub", MarkVal::Bool(true))]),
                    "mark" => push_inline(&mut inline, &name, cur_len, vec![("highlight", MarkVal::Str("#ffff00".to_string()))]),
                    "code" => {
                        // Inline `<code>` → mono mark; inside `<pre>` it's structural.
                        if pre_depth > 0 {
                            push_inline(&mut inline, &name, cur_len, vec![]);
                        } else {
                            push_inline(&mut inline, &name, cur_len, vec![("fontFamily", MarkVal::Str("mono".to_string()))]);
                        }
                    }
                    "a" => {
                        let href = attr_value(&attrs, "href").unwrap_or_default();
                        let marks = if href.is_empty() {
                            vec![]
                        } else {
                            vec![("link", MarkVal::Str(href))]
                        };
                        push_inline(&mut inline, &name, cur_len, marks);
                    }
                    "span" | "font" => {
                        let marks = attr_value(&attrs, "style")
                            .map(|s| marks_from_style(&s))
                            .unwrap_or_default();
                        push_inline(&mut inline, &name, cur_len, marks);
                    }
                    // Unknown inline/other tags: pass through (their text survives).
                    _ => {}
                }
            }
            Token::Close { name } => match name.as_str() {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "td" | "th" => {
                    finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                    cur_block = None;
                    cur_len = 0;
                    if matches!(name.as_str(), "td" | "th") {
                        tcol += 1;
                    }
                }
                "p" => {
                    if quote_depth == 0 {
                        finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                        cur_block = None;
                        cur_len = 0;
                    }
                    // Inside a quote, keep the quote block current across `</p>`.
                }
                "blockquote" => {
                    finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                    quote_depth = quote_depth.saturating_sub(1);
                    cur_block = None;
                    cur_len = 0;
                }
                "pre" => {
                    finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
                    pre_depth = pre_depth.saturating_sub(1);
                    cur_block = None;
                    cur_len = 0;
                }
                "ul" | "ol" => {
                    lists.pop();
                }
                "tr" => {
                    in_row = true; // stay in row; next <tr> bumps trow
                }
                "table" => {
                    table_id = None;
                    in_row = false;
                }
                // Close the nearest matching open inline wrapper (robust to mis-nesting).
                "strong" | "b" | "em" | "i" | "u" | "ins" | "s" | "strike" | "del" | "sup"
                | "sub" | "mark" | "code" | "a" | "span" | "font" => {
                    close_inline(&mut inline, &mut pending, &name, &cur_block, cur_len);
                }
                _ => {}
            },
            Token::Text(t) => {
                if t.is_empty() {
                    continue;
                }
                if pre_depth > 0 {
                    // Verbatim inside <pre>: keep every char (including whitespace).
                    if let Some(b) = cur_block.as_ref() {
                        doc.splice_text(b, cur_len, 0, &t);
                        cur_len += t.chars().count();
                    }
                    continue;
                }
                // Collapse runs of HTML whitespace to single spaces (HTML rules).
                let collapsed = collapse_ws(&t);
                if collapsed.is_empty() {
                    continue;
                }
                // Open a default paragraph if text appears outside any block.
                if cur_block.is_none() {
                    // Skip leading inter-block whitespace (don't create empty paras).
                    if collapsed.trim().is_empty() {
                        continue;
                    }
                    open_block(doc, &mut prev_block, &mut cur_block, &mut cur_len, "paragraph");
                }
                if let Some(b) = cur_block.as_ref() {
                    doc.splice_text(b, cur_len, 0, &collapsed);
                    cur_len += collapsed.chars().count();
                }
            }
        }
    }

    // Close any inline wrappers still open at EOF, then apply gathered marks.
    finish_inline(&mut inline, &mut pending, &cur_block, cur_len);
    for pm in pending {
        if pm.end <= pm.start {
            continue;
        }
        match pm.value {
            MarkVal::Bool(b) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, b),
            MarkVal::Str(s) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, s.as_str()),
            MarkVal::Int(n) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, n),
        }
    }
}

/// Open a fresh top-level block, anchored after the previous one, and make it the
/// current text target. Returns its id.
fn open_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    kind: &str,
) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

/// Insert an `<img>` as an `image` block. A `data:…;base64,…` src is decoded into a
/// content-addressed blob (the block carries a `blob` attr = blake3 hex); an
/// `http(s)`/relative src is kept on a `src` attr with the alt text as the body.
fn insert_image(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    attrs: &str,
) {
    let src = attr_value(attrs, "src").unwrap_or_default();
    let alt = attr_value(attrs, "alt").unwrap_or_default();
    let id = doc.insert_block(prev.as_deref(), "image");
    *prev = Some(id.clone());
    // An image block does not become the inline text cursor.
    *cur = None;
    *cur_len = 0;

    if !alt.is_empty() {
        doc.splice_text(&id, 0, 0, &alt);
    }
    if let Some(bytes) = decode_data_uri(&src) {
        let hash = doc.put_blob(&bytes);
        doc.set_attr(&id, "blob", hash.as_str());
    } else if !src.is_empty() {
        // Keep an http/relative src for later resolution; the alt is the fallback.
        doc.set_attr(&id, "src", src.as_str());
    }
}

/// Decode a `data:[mime][;base64],payload` URI into bytes. Returns `None` for a
/// non-data URI or an undecodable payload. Only base64 payloads are supported
/// (the form `dochtml` emits).
fn decode_data_uri(src: &str) -> Option<Vec<u8>> {
    let rest = src.strip_prefix("data:").or_else(|| src.strip_prefix("DATA:"))?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    if !meta.to_ascii_lowercase().contains("base64") {
        return None;
    }
    base64_decode(payload)
}

/// Minimal standard-alphabet base64 decoder (no external dep). Ignores ASCII
/// whitespace; returns `None` on an invalid alphabet character.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &b in s.as_bytes() {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = val(b)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Collapse runs of HTML whitespace to single spaces. Only ASCII whitespace
/// (space, tab, CR, LF, form feed) collapses — a non-breaking space (`&nbsp;` →
/// U+00A0) is a meaningful character and is preserved verbatim.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(c);
            last_ws = false;
        }
    }
    out
}

/// Push an open inline wrapper onto the stack.
fn push_inline(inline: &mut Vec<OpenInline>, tag: &str, start: usize, marks: Vec<(&'static str, MarkVal)>) {
    inline.push(OpenInline { tag: tag.to_string(), marks, start });
}

/// Close the nearest open wrapper matching `tag` and emit its marks over
/// `[start, cur_len)` into `pending`. Robust to mis-nesting.
fn close_inline(
    inline: &mut Vec<OpenInline>,
    pending: &mut Vec<PendingMark>,
    tag: &str,
    cur_block: &Option<String>,
    cur_len: usize,
) {
    // Treat `<b>` and `<strong>` (and `<i>`/`<em>`) as interchangeable on close.
    let matches = |t: &str| -> bool {
        t == tag
            || (matches!(tag, "b" | "strong") && matches!(t, "b" | "strong"))
            || (matches!(tag, "i" | "em") && matches!(t, "i" | "em"))
            || (matches!(tag, "s" | "strike" | "del") && matches!(t, "s" | "strike" | "del"))
    };
    if let Some(pos) = inline.iter().rposition(|w| matches(&w.tag)) {
        let w = inline.remove(pos);
        if let Some(block) = cur_block {
            for (name, value) in w.marks {
                pending.push(PendingMark { block: block.clone(), start: w.start, end: cur_len, name, value });
            }
        }
    }
}

/// Close every still-open inline wrapper, emitting marks up to `cur_len`. Called at
/// block boundaries (an unclosed `<b>` shouldn't bleed into the next block).
fn finish_inline(inline: &mut Vec<OpenInline>, pending: &mut Vec<PendingMark>, cur_block: &Option<String>, cur_len: usize) {
    while let Some(w) = inline.pop() {
        if let Some(block) = cur_block {
            for (name, value) in w.marks {
                pending.push(PendingMark { block: block.clone(), start: w.start, end: cur_len, name, value });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::DocSnapshot;
    use crate::Document;

    fn import(src: &str) -> DocSnapshot {
        let mut d = Document::new();
        HtmlCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
        d.materialize()
    }

    #[test]
    fn detect_recognizes_html_signature() {
        assert_eq!(HtmlCodec.detect(b"<!doctype html>\n<html>"), Confidence::High);
        assert_eq!(HtmlCodec.detect(b"  <html lang=\"en\">"), Confidence::High);
        assert_eq!(HtmlCodec.detect(b"<p>hi</p>"), Confidence::Medium);
        assert_eq!(HtmlCodec.detect(b"plain text"), Confidence::None);
    }

    #[test]
    fn headings_and_paragraph() {
        let snap = import("<h1>Title</h1><h2>Sub</h2><p>Body text</p>");
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&crate::AttrVal::Int(1)));
        assert_eq!(snap.blocks[1].kind, "heading");
        assert_eq!(snap.blocks[1].attrs.get("level"), Some(&crate::AttrVal::Int(2)));
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "Body text");
    }

    #[test]
    fn bold_italic_underline_strike_marks() {
        let snap = import("<p>a<strong>b</strong><em>c</em><u>d</u><s>e</s></p>");
        let p = &snap.blocks[0];
        assert_eq!(p.text, "abcde");
        assert!(p.marks.iter().any(|m| m.name == "strong" && m.start == 1 && m.end == 2), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "em" && m.start == 2 && m.end == 3), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "underline" && m.start == 3 && m.end == 4), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "strike" && m.start == 4 && m.end == 5), "{:?}", p.marks);
    }

    #[test]
    fn b_and_i_aliases_map_to_strong_em() {
        let snap = import("<p><b>x</b> <i>y</i></p>");
        let p = &snap.blocks[0];
        assert_eq!(p.text, "x y");
        assert!(p.marks.iter().any(|m| m.name == "strong"), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "em"), "{:?}", p.marks);
    }

    #[test]
    fn link_becomes_mark() {
        let snap = import(r#"<p>See <a href="https://qos.dev/">the docs</a> now</p>"#);
        let p = &snap.blocks[0];
        assert_eq!(p.text, "See the docs now");
        let link = p.marks.iter().find(|m| m.name == "link").expect("link mark");
        assert_eq!(link.value, crate::document::MarkValue::Str("https://qos.dev/".into()));
        assert_eq!(&p.text[link.start..link.end], "the docs");
    }

    #[test]
    fn unordered_and_ordered_list_items() {
        let snap = import("<ul><li>one</li><li>two</li></ul><ol><li>first</li></ol>");
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 3, "{snap:?}");
        assert_eq!(items[0].text, "one");
        assert_eq!(items[0].attrs.get("ordered"), Some(&crate::AttrVal::Bool(false)));
        assert_eq!(items[2].attrs.get("ordered"), Some(&crate::AttrVal::Bool(true)));
    }

    #[test]
    fn nested_lists_carry_level() {
        let snap = import("<ul><li>top</li><ul><li>sub</li></ul></ul>");
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2);
        // The nested item is one level deeper.
        let sub = items.iter().find(|b| b.text == "sub").expect("sub item");
        assert_eq!(sub.attrs.get("level"), Some(&crate::AttrVal::Int(1)), "{:?}", sub.attrs);
    }

    #[test]
    fn blockquote_becomes_quote_block() {
        let snap = import("<blockquote><p>quoted line</p></blockquote>");
        let q = snap.blocks.iter().find(|b| b.kind == "quote").expect("quote block");
        assert!(q.text.contains("quoted line"), "got {:?}", q.text);
    }

    #[test]
    fn table_cells_tagged_table_row_col() {
        let snap = import("<table><tr><td>a</td><td>b</td></tr><tr><td>1</td><td>2</td></tr></table>");
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "{snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "1", "2"]);
        assert_eq!(cells[0].attrs.get("row"), Some(&crate::AttrVal::Int(0)));
        assert_eq!(cells[0].attrs.get("col"), Some(&crate::AttrVal::Int(0)));
        assert_eq!(cells[3].attrs.get("row"), Some(&crate::AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&crate::AttrVal::Int(1)));
    }

    #[test]
    fn pre_code_is_verbatim_code_block() {
        let snap = import("<pre><code>let x = 1;\nlet y = 2;\n</code></pre>");
        let code = snap
            .blocks
            .iter()
            .find(|b| matches!(b.attrs.get("code"), Some(&crate::AttrVal::Bool(true))))
            .expect("code block");
        assert_eq!(code.text, "let x = 1;\nlet y = 2;\n");
    }

    #[test]
    fn entities_are_unescaped() {
        let snap = import("<p>a &amp; b &lt; c &gt; d &#65; &#x42; &nbsp;e</p>");
        let p = &snap.blocks[0];
        assert_eq!(p.text, "a & b < c > d A B \u{a0}e");
    }

    #[test]
    fn script_and_style_are_skipped() {
        let snap = import("<p>before</p><script>alert('x'); 1 < 2;</script><style>p{color:red}</style><p>after</p>");
        let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
        assert_eq!(texts, vec!["before".to_string(), "after".to_string()], "{snap:?}");
    }

    #[test]
    fn head_is_skipped() {
        let snap = import("<head><title>Doc Title</title></head><body><p>Body</p></body>");
        let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
        assert_eq!(texts, vec!["Body".to_string()], "title must not leak into body: {snap:?}");
    }

    #[test]
    fn span_style_carries_color_size_font() {
        let snap = import(r#"<p><span style="color: #ff0000; font-size: 20pt; font-family: serif">styled</span></p>"#);
        let p = &snap.blocks[0];
        assert_eq!(p.text, "styled");
        assert!(
            p.marks.iter().any(|m| m.name == "color" && m.value == crate::document::MarkValue::Str("#ff0000".into())),
            "{:?}", p.marks
        );
        assert!(
            p.marks.iter().any(|m| m.name == "fontSize" && m.value == crate::document::MarkValue::Int(20)),
            "{:?}", p.marks
        );
        assert!(
            p.marks.iter().any(|m| m.name == "fontFamily" && m.value == crate::document::MarkValue::Str("serif".into())),
            "{:?}", p.marks
        );
    }

    #[test]
    fn background_becomes_highlight() {
        let snap = import(r#"<p><span style="background: #ffff00">hi</span></p>"#);
        let p = &snap.blocks[0];
        assert!(
            p.marks.iter().any(|m| m.name == "highlight" && m.value == crate::document::MarkValue::Str("#ffff00".into())),
            "{:?}", p.marks
        );
    }

    #[test]
    fn sup_and_sub_marks() {
        let snap = import("<p>x<sup>2</sup>y<sub>n</sub></p>");
        let p = &snap.blocks[0];
        assert_eq!(p.text, "x2yn");
        assert!(p.marks.iter().any(|m| m.name == "super" && m.start == 1 && m.end == 2), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "sub" && m.start == 3 && m.end == 4), "{:?}", p.marks);
    }

    #[test]
    fn data_uri_image_becomes_blob() {
        // A 1-byte payload "A" (base64 "QQ==").
        let snap = import(r#"<p><img src="data:image/png;base64,QQ==" alt="pic"></p>"#);
        let img = snap.blocks.iter().find(|b| b.kind == "image").expect("image block");
        assert_eq!(img.text, "pic");
        let blob = match img.attrs.get("blob") {
            Some(crate::AttrVal::Str(s)) => s.clone(),
            other => panic!("expected blob attr, got {other:?}"),
        };
        // The blob is stored under its hash and decodes back to "A".
        assert_eq!(snap.blobs.get(&blob).map(|b| b.as_slice()), Some(b"A".as_slice()), "{:?}", snap.blobs);
    }

    #[test]
    fn http_image_keeps_src() {
        let snap = import(r#"<img src="https://example.com/p.png" alt="remote">"#);
        let img = snap.blocks.iter().find(|b| b.kind == "image").expect("image block");
        assert_eq!(img.text, "remote");
        assert_eq!(img.attrs.get("src"), Some(&crate::AttrVal::Str("https://example.com/p.png".into())));
    }

    #[test]
    fn whitespace_is_collapsed() {
        let snap = import("<p>a   b\n  c</p>");
        assert_eq!(snap.blocks[0].text, "a b c");
    }

    #[test]
    fn unclosed_tags_are_tolerated() {
        // No closing `</b>`/`</p>`; the importer must still produce sane blocks.
        let snap = import("<p>start <b>bold and more");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "start bold and more");
        assert!(p.marks.iter().any(|m| m.name == "strong"), "unclosed bold still marks: {:?}", p.marks);
    }

    #[test]
    fn export_is_unsupported() {
        let d = Document::new();
        let err = HtmlCodec.export(d.as_ref()).unwrap_err();
        assert!(matches!(err, CodecError::Unsupported(_)), "got {err:?}");
    }
}
