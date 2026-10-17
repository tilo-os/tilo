//! Built-in HTML **import** codec — parses `.html`/`.htm` onto the canonical
//! rich-text model ([`crate::export_html`] handles export). HTML in the wild isn't strict
//! XML, so this hand-rolls a tolerant tokenizer: never errors on malformed
//! markup, degrades unknown tags to text, unescapes entities itself.
//!
//! Tag → model: `<h1-6>`→`heading`+`level`; `<p>`→`paragraph`; `<blockquote>`→
//! `quote`; `<ul>`/`<ol>`/`<li>`→`list_item` (`ordered`, nesting `level`);
//! `<table><tr><td|th>`→`cell` tagged `table`/`row`/`col`; `<pre>`/`<pre><code>`→
//! `code`=true; `<img src>`→`image` (`data:` → content-addressed blob, else `src` attr).
//!
//! Inline → mark: `<strong>`/`<b>`→`strong`; `<em>`/`<i>`→`em`; `<u>`→`underline`;
//! `<s>`/`<del>`/`<strike>`→`strike`; `<a href>`→`link`; `<span style>` maps CSS
//! colour/background/font-size/font-family; `<sup>`/`<sub>`→`super`/`sub`;
//! inline `<code>`→`fontFamily`=mono.
//!
//! `<head>`/`<script>`/`<style>` is skipped. `export` is `Unsupported`: HTML
//! export is [`crate::export_html::render_html`], which needs a `DocSnapshot`,
//! not the `DocRef` a `FormatProvider` gets.

use crate::format::{CodecError, FormatProvider};
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};

pub struct HtmlCodec;

impl FormatProvider for HtmlCodec {
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        doc.bulk(|doc| {
            let src = String::from_utf8_lossy(bytes);
            import_html(&src, doc, inputs);
            Ok(())
        })
    }

    fn export(&self, _doc: DocRef, _content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
        // The HTML writer renders a materialized `DocSnapshot`, not a `DocRef`.
        Err(CodecError::Unsupported(
            "HTML export is provided by tilo_doc::export_html::render_html, not the qdoc codec"
                .to_string(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A start tag: lowercased name + raw attrs (between the name and `>`/`/>`).
    Open {
        name: String,
        attrs: String,
        self_closing: bool,
    },
    Close {
        name: String,
    },
    Text(String),
}

/// Tokenizes HTML into a flat tag/text stream (tolerant: an unterminated `<` is
/// literal text; `<script>`/`<style>` bodies are consumed raw and dropped).
fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let len = chars.len();
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

    while i < len {
        let ch = char_at(&chars, i);
        if ch != '<' {
            text.push(ch);
            i = i.saturating_add(1);
            continue;
        }
        if starts_with_at(&chars, i, "<!--") {
            flush_text!();
            i = skip_comment(&chars, i);
            continue;
        }
        if matches!(char_at(&chars, i.saturating_add(1)), '!' | '?') {
            flush_text!();
            i = skip_declaration(&chars, i);
            continue;
        }
        let next = char_at(&chars, i.saturating_add(1));
        let is_tag = i.saturating_add(1) < len && (next == '/' || next.is_ascii_alphabetic());
        if !is_tag {
            text.push(ch);
            i = i.saturating_add(1);
            continue;
        }
        let Some((raw, end)) = scan_tag(&chars, i) else {
            text.push(ch);
            i = i.saturating_add(1);
            continue;
        };
        flush_text!();
        if let Some(close) = raw.strip_prefix('/') {
            let name = close
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !name.is_empty() {
                out.push(Token::Close { name });
            }
            i = end.saturating_add(1);
            continue;
        }
        let self_closing = raw.trim_end().ends_with('/');
        let body = raw.trim_end().trim_end_matches('/');
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let name = body.get(..name_end).unwrap_or("").to_ascii_lowercase();
        let attrs = body.get(name_end..).unwrap_or("").trim().to_string();
        // `<script>`/`<style>` bodies are raw text (CDATA-like); consume to the matching close tag and drop both.
        if (name == "script" || name == "style") && !self_closing {
            i = skip_raw_body(&chars, end.saturating_add(1), &name);
            continue;
        }
        out.push(Token::Open {
            name,
            attrs,
            self_closing,
        });
        i = end.saturating_add(1);
    }
    flush_text!();
    out
}

/// A safe, panic-free stand-in for `chars[i]`: out-of-bounds reads as NUL, which
/// never matches any tag/quote delimiter this tokenizer looks for.
fn char_at(chars: &[char], i: usize) -> char {
    chars.get(i).copied().unwrap_or('\0')
}

/// Advances past a `<!--...-->` comment; tolerant of an unterminated one (runs to EOF).
fn skip_comment(chars: &[char], i: usize) -> usize {
    let len = chars.len();
    let mut j = i.saturating_add(4);
    while j < len && !starts_with_at(chars, j, "-->") {
        j = j.saturating_add(1);
    }
    j.saturating_add(3).min(len)
}

/// Advances past a `<!...>`/`<?...>` declaration or processing instruction.
fn skip_declaration(chars: &[char], i: usize) -> usize {
    let len = chars.len();
    let mut j = i;
    while j < len && char_at(chars, j) != '>' {
        j = j.saturating_add(1);
    }
    j.saturating_add(1).min(len)
}

/// Scans the tag body opened at `i` (pointing at `<`) up to its matching `>`,
/// honouring quoted attribute values so a quoted `>` doesn't end the tag early.
/// Returns the raw text between `<` and `>` plus the index of `>`, or `None`
/// if the tag is unterminated.
fn scan_tag(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let mut j = i.saturating_add(1);
    let mut quote: Option<char> = None;
    let end = loop {
        if j >= len {
            return None;
        }
        let cj = char_at(chars, j);
        match quote {
            Some(q) if cj == q => quote = None,
            None if cj == '"' || cj == '\'' => quote = Some(cj),
            None if cj == '>' => break j,
            _ => {}
        }
        j = j.saturating_add(1);
    };
    let raw = chars.get(i.saturating_add(1)..end)?.iter().collect();
    Some((raw, end))
}

/// Consumes a `<script>`/`<style>` body up to and including its matching close tag.
fn skip_raw_body(chars: &[char], start: usize, name: &str) -> usize {
    let len = chars.len();
    let close_tag = format!("</{name}");
    let mut j = start;
    while j < len && !starts_with_ci(chars, j, &close_tag) {
        j = j.saturating_add(1);
    }
    while j < len && char_at(chars, j) != '>' {
        j = j.saturating_add(1);
    }
    j.saturating_add(1).min(len)
}

fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    let Some(end) = i.checked_add(pc.len()) else {
        return false;
    };
    chars.get(i..end) == Some(pc.as_slice())
}

fn starts_with_ci(chars: &[char], i: usize, pat: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    let Some(end) = i.checked_add(pc.len()) else {
        return false;
    };
    match chars.get(i..end) {
        Some(slice) => slice
            .iter()
            .zip(pc.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b)),
        None => false,
    }
}

/// Unescapes HTML entities: numeric (`&#NN;`, `&#xHH;`) and common named ones;
/// unknown entities and a bare `&` pass through unchanged.
fn unescape_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < len {
        let ch = char_at(&chars, i);
        if ch != '&' {
            out.push(ch);
            i = i.saturating_add(1);
            continue;
        }
        let mut j = i.saturating_add(1);
        while j < len
            && j.saturating_sub(i) <= 12
            && !matches!(char_at(&chars, j), ';' | '&')
            && !char_at(&chars, j).is_whitespace()
        {
            j = j.saturating_add(1);
        }
        if j < len
            && char_at(&chars, j) == ';'
            && let Some(ent_chars) = chars.get(i.saturating_add(1)..j)
        {
            let ent: String = ent_chars.iter().collect();
            if let Some(decoded) = decode_entity(&ent) {
                out.push_str(&decoded);
                i = j.saturating_add(1);
                continue;
            }
        }
        out.push('&');
        i = i.saturating_add(1);
    }
    out
}

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

/// Parses a raw attribute string (`a="1" b='2' c=3 d`) into `(name_lower, value)` pairs; valueless attrs get an empty value.
fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut out = Vec::new();
    while i < len {
        while i < len && (char_at(&chars, i).is_whitespace() || char_at(&chars, i) == '/') {
            i = i.saturating_add(1);
        }
        if i >= len {
            break;
        }
        let start = i;
        while i < len
            && !char_at(&chars, i).is_whitespace()
            && char_at(&chars, i) != '='
            && char_at(&chars, i) != '/'
        {
            i = i.saturating_add(1);
        }
        let name: String = chars
            .get(start..i)
            .unwrap_or(&[])
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            i = i.saturating_add(1);
            continue;
        }
        while i < len && char_at(&chars, i).is_whitespace() {
            i = i.saturating_add(1);
        }
        let mut value = String::new();
        if i < len && char_at(&chars, i) == '=' {
            i = i.saturating_add(1);
            while i < len && char_at(&chars, i).is_whitespace() {
                i = i.saturating_add(1);
            }
            if i < len && matches!(char_at(&chars, i), '"' | '\'') {
                let quote = char_at(&chars, i);
                i = i.saturating_add(1);
                let vs = i;
                while i < len && char_at(&chars, i) != quote {
                    i = i.saturating_add(1);
                }
                value = chars.get(vs..i).unwrap_or(&[]).iter().collect();
                i = i.saturating_add(1).min(len);
            } else {
                let vs = i;
                while i < len && !char_at(&chars, i).is_whitespace() {
                    i = i.saturating_add(1);
                }
                value = chars.get(vs..i).unwrap_or(&[]).iter().collect();
            }
        }
        out.push((name, unescape_entities(&value)));
    }
    out
}

/// The value of attribute `key` (case-insensitive) in a raw attr string, if present.
fn attr_value(raw: &str, key: &str) -> Option<String> {
    parse_attrs(raw)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// Parses a `style="…"` string into the model run-marks it implies (mirrors the markdown codec's `marks_from_span_style`).
fn marks_from_style(style: &str) -> Vec<(&'static str, MarkVal)> {
    let mut out: Vec<(&'static str, MarkVal)> = Vec::new();
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        match prop.as_str() {
            "color" => out.push(("color", MarkVal::Str(normalize_css_color(val)))),
            "background" | "background-color" => {
                out.push(("highlight", MarkVal::Str(normalize_css_color(val))));
            }
            "font-size" => {
                let num = val
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%')
                    .trim();
                if let Ok(f) = num.parse::<f64>() {
                    let clamped = f.round().clamp(0.0, 10_000.0);
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "clamped to [0, 10_000] above; no CSS font-size is meaningfully larger"
                    )]
                    let px = clamped as i64;
                    out.push(("fontSize", MarkVal::Int(px)));
                }
            }
            "font-family" => out.push((
                "fontFamily",
                MarkVal::Str(crate::generic_font_family(val).to_string()),
            )),
            "font-weight" => {
                if val == "bold" || val.parse::<i32>().is_ok_and(|w| w >= 600) {
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

/// Normalises a CSS colour to canonical `#rrggbb` (lower-case hex, common keywords, else passed through trimmed).
fn normalize_css_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return format!("#{}", hex.to_ascii_lowercase());
        }
        if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
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

/// A mark value: boolean toggle, a string payload, or an integer (fontSize).
#[derive(Clone)]
enum MarkVal {
    Bool(bool),
    Str(String),
    Int(i64),
}

/// An open inline wrapper: the marks it contributes + the char index where it opened; closed and applied on its end tag.
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

/// One open list context: whether it's ordered, so nested `<li>`s carry the right `ordered` + `level`.
struct ListCtx {
    ordered: bool,
}

/// All of `import_html`'s parse state, threaded through the per-token handlers.
#[derive(Default)]
struct ImportState {
    // Images carried inline as `data:` URIs, for the caller's content store.
    inputs: ContentInputs,
    prev_block: Option<String>,
    cur_block: Option<String>,
    cur_len: usize,
    inline: Vec<OpenInline>,
    pending: Vec<PendingMark>,
    lists: Vec<ListCtx>,
    table_id: Option<String>,
    trow: usize,
    tcol: usize,
    table_seq: usize,
    // Whether we've seen a `<tr>` yet — a leading `<td>` with no `<tr>` still gets row 0.
    in_row: bool,
    // Skip-content: inside `<head>`/`<title>` etc., drop all text until this tag's close.
    skip_until: Option<String>,
    // Quote depth: while > 0 (inside a `<blockquote>`), a bare `<p>` flows into the quote block instead of opening a fresh paragraph.
    quote_depth: usize,
    // Pre depth: while > 0 (inside `<pre>`), text is verbatim and `<code>` adds no mono mark.
    pre_depth: usize,
}

fn import_html(src: &str, doc: &mut DocMut, inputs: &mut ContentInputs) {
    let tokens = tokenize(src);
    let mut st = ImportState::default();

    for tok in tokens {
        if let Some(skip) = &st.skip_until {
            if let Token::Close { name } = &tok
                && name == skip
            {
                st.skip_until = None;
            }
            continue;
        }

        match tok {
            Token::Open {
                name,
                attrs,
                self_closing,
            } => handle_open_tag(doc, &mut st, &name, &attrs, self_closing),
            Token::Close { name } => handle_close_tag(&mut st, &name),
            Token::Text(t) => handle_text(doc, &mut st, &t),
        }
    }

    // Close any inline wrappers still open at EOF, then apply gathered marks.
    finish_inline_now(&mut st);
    inputs.append(&mut st.inputs);
    apply_pending_marks(doc, st.pending);
}

fn apply_pending_marks(doc: &mut DocMut, pending: Vec<PendingMark>) {
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

fn handle_open_tag(
    doc: &mut DocMut,
    st: &mut ImportState,
    name: &str,
    attrs: &str,
    self_closing: bool,
) {
    match name {
        "head" | "title" => {
            if !self_closing {
                st.skip_until = Some(name.to_string());
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            finish_inline_now(st);
            let level = name
                .get(1..)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(1)
                .clamp(1, 6);
            let id = open_block(doc, st, "heading");
            doc.set_attr(&id, "level", level);
        }
        "p" => {
            if st.quote_depth > 0 {
                // Inside a blockquote, `<p>` text flows into the quote block, opened lazily if needed.
                if st.cur_block.is_none() {
                    open_block(doc, st, "quote");
                }
            } else {
                finish_inline_now(st);
                open_block(doc, st, "paragraph");
            }
        }
        "blockquote" => {
            finish_inline_now(st);
            // Open the quote block now; nested `<p>`s flow into it.
            open_block(doc, st, "quote");
            st.quote_depth = st.quote_depth.saturating_add(1);
        }
        "br" => {
            if let Some(b) = st.cur_block.clone() {
                doc.splice_text(&b, st.cur_len, 0, "\n");
                st.cur_len = st.cur_len.saturating_add(1);
            }
        }
        "ul" => st.lists.push(ListCtx { ordered: false }),
        "ol" => st.lists.push(ListCtx { ordered: true }),
        "li" => {
            finish_inline_now(st);
            let ordered = st.lists.last().is_some_and(|l| l.ordered);
            let level = st.lists.len().saturating_sub(1);
            let id = open_block(doc, st, "list_item");
            doc.set_attr(&id, "ordered", ordered);
            if level > 0 {
                doc.set_attr(&id, "level", i64::try_from(level).unwrap_or(i64::MAX));
            }
        }
        "table" | "tr" | "td" | "th" => open_table_tag(doc, st, name),
        "pre" => {
            finish_inline_now(st);
            let id = open_block(doc, st, "paragraph");
            doc.set_attr(&id, "code", true);
            st.pre_depth = st.pre_depth.saturating_add(1);
        }
        "img" => {
            finish_inline_now(st);
            insert_image(doc, st, attrs);
        }
        "strong" | "b" | "em" | "i" | "u" | "ins" | "s" | "strike" | "del" | "sup" | "sub"
        | "mark" | "code" | "a" | "span" | "font" => open_inline_tag(st, name, attrs),
        // Structural containers (html/body/div/section/…), `hr`, and
        // unknown/other tags: no modeled construct, so their text just
        // flows through.
        _ => {}
    }
}

fn open_table_tag(doc: &mut DocMut, st: &mut ImportState, name: &str) {
    match name {
        "table" => {
            st.table_seq = st.table_seq.saturating_add(1);
            st.table_id = Some(format!("t{}", st.table_seq));
            st.trow = 0;
            st.in_row = false;
        }
        "tr" => {
            if st.in_row {
                st.trow = st.trow.saturating_add(1);
            }
            st.in_row = true;
            st.tcol = 0;
        }
        "td" | "th" => {
            if let Some(tid) = st.table_id.clone() {
                finish_inline_now(st);
                let id = open_block(doc, st, "cell");
                doc.set_attr(&id, "table", tid.as_str());
                doc.set_attr(&id, "row", i64::try_from(st.trow).unwrap_or(i64::MAX));
                doc.set_attr(&id, "col", i64::try_from(st.tcol).unwrap_or(i64::MAX));
            }
        }
        _ => {}
    }
}

fn open_inline_tag(st: &mut ImportState, name: &str, attrs: &str) {
    match name {
        "strong" | "b" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("strong", MarkVal::Bool(true))],
        ),
        "em" | "i" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("em", MarkVal::Bool(true))],
        ),
        "u" | "ins" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("underline", MarkVal::Bool(true))],
        ),
        "s" | "strike" | "del" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("strike", MarkVal::Bool(true))],
        ),
        "sup" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("super", MarkVal::Bool(true))],
        ),
        "sub" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("sub", MarkVal::Bool(true))],
        ),
        "mark" => push_inline(
            &mut st.inline,
            name,
            st.cur_len,
            vec![("highlight", MarkVal::Str("#ffff00".to_string()))],
        ),
        "code" => {
            // Inline `<code>` → mono mark; inside `<pre>` it's structural.
            if st.pre_depth > 0 {
                push_inline(&mut st.inline, name, st.cur_len, vec![]);
            } else {
                push_inline(
                    &mut st.inline,
                    name,
                    st.cur_len,
                    vec![("fontFamily", MarkVal::Str("mono".to_string()))],
                );
            }
        }
        "a" => {
            let href = attr_value(attrs, "href").unwrap_or_default();
            let marks = if href.is_empty() {
                vec![]
            } else {
                vec![("link", MarkVal::Str(href))]
            };
            push_inline(&mut st.inline, name, st.cur_len, marks);
        }
        "span" | "font" => {
            let marks = attr_value(attrs, "style")
                .map(|s| marks_from_style(&s))
                .unwrap_or_default();
            push_inline(&mut st.inline, name, st.cur_len, marks);
        }
        _ => {}
    }
}

fn handle_close_tag(st: &mut ImportState, name: &str) {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "td" | "th" => {
            finish_inline_now(st);
            st.cur_block = None;
            st.cur_len = 0;
            if matches!(name, "td" | "th") {
                st.tcol = st.tcol.saturating_add(1);
            }
        }
        "p" => {
            if st.quote_depth == 0 {
                finish_inline_now(st);
                st.cur_block = None;
                st.cur_len = 0;
            }
            // Inside a quote, keep the quote block current across `</p>`.
        }
        "blockquote" => {
            finish_inline_now(st);
            st.quote_depth = st.quote_depth.saturating_sub(1);
            st.cur_block = None;
            st.cur_len = 0;
        }
        "pre" => {
            finish_inline_now(st);
            st.pre_depth = st.pre_depth.saturating_sub(1);
            st.cur_block = None;
            st.cur_len = 0;
        }
        "ul" | "ol" => {
            st.lists.pop();
        }
        "tr" => {
            st.in_row = true; // stay in row; next <tr> bumps trow
        }
        "table" => {
            st.table_id = None;
            st.in_row = false;
        }
        // Close the nearest matching open inline wrapper (robust to mis-nesting).
        "strong" | "b" | "em" | "i" | "u" | "ins" | "s" | "strike" | "del" | "sup" | "sub"
        | "mark" | "code" | "a" | "span" | "font" => {
            close_inline(
                &mut st.inline,
                &mut st.pending,
                name,
                st.cur_block.as_deref(),
                st.cur_len,
            );
        }
        _ => {}
    }
}

fn handle_text(doc: &mut DocMut, st: &mut ImportState, t: &str) {
    if t.is_empty() {
        return;
    }
    if st.pre_depth > 0 {
        if let Some(b) = st.cur_block.clone() {
            doc.splice_text(&b, st.cur_len, 0, t);
            st.cur_len = st.cur_len.saturating_add(t.chars().count());
        }
        return;
    }
    // Collapse runs of HTML whitespace to single spaces (HTML rules).
    let collapsed = collapse_ws(t);
    if collapsed.is_empty() {
        return;
    }
    if st.cur_block.is_none() {
        if collapsed.trim().is_empty() {
            return;
        }
        open_block(doc, st, "paragraph");
    }
    if let Some(b) = st.cur_block.clone() {
        doc.splice_text(&b, st.cur_len, 0, &collapsed);
        st.cur_len = st.cur_len.saturating_add(collapsed.chars().count());
    }
}

/// Opens a fresh top-level block anchored after the previous one, and makes it the current text target; returns its id.
fn open_block(doc: &mut DocMut, st: &mut ImportState, kind: &str) -> String {
    let id = doc.insert_block(st.prev_block.as_deref(), kind);
    st.prev_block = Some(id.clone());
    st.cur_block = Some(id.clone());
    st.cur_len = 0;
    id
}

/// Inserts an `<img>` as an `image` block: a `data:…;base64,…` src decodes into a content-addressed blob (`blob` attr), else kept as `src`.
fn insert_image(doc: &mut DocMut, st: &mut ImportState, attrs: &str) {
    let src = attr_value(attrs, "src").unwrap_or_default();
    let alt = attr_value(attrs, "alt").unwrap_or_default();
    let id = doc.insert_block(st.prev_block.as_deref(), "image");
    st.prev_block = Some(id.clone());
    // An image block does not become the inline text cursor.
    st.cur_block = None;
    st.cur_len = 0;

    if !alt.is_empty() {
        doc.splice_text(&id, 0, 0, &alt);
    }
    if let Some(bytes) = decode_data_uri(&src) {
        let hash = doc.attach(&bytes, &mut st.inputs);
        doc.set_attr(&id, "blob", hash.as_str());
    } else if !src.is_empty() {
        // Keep an http/relative src for later resolution; the alt is the fallback.
        doc.set_attr(&id, "src", src.as_str());
    }
}

/// Decodes a `data:[mime][;base64],payload` URI into bytes; `None` for a non-data
/// URI or undecodable payload (base64 only, matching `export_html`).
fn decode_data_uri(src: &str) -> Option<Vec<u8>> {
    let rest = src
        .strip_prefix("data:")
        .or_else(|| src.strip_prefix("DATA:"))?;
    let comma = rest.find(',')?;
    let meta = rest.get(..comma)?;
    let payload = rest.get(comma.checked_add(1)?..)?;
    if !meta.to_ascii_lowercase().contains("base64") {
        return None;
    }
    base64_decode(payload)
}

/// Decodes a standard-alphabet base64 payload as HTML authors write it: ASCII
/// whitespace is ignored, padding is optional and decoding stops at the first
/// `=`, and non-zero trailing bits or a dangling final character are dropped.
/// `None` when any character outside the standard alphabet is present.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );

    let mut compact: Vec<u8> = s
        .bytes()
        .take_while(|&b| b != b'=')
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    // A lone final character carries only six bits -- not a whole byte. It is
    // still validated, then dropped, rather than failing the whole payload.
    if compact.len() % 4 == 1 {
        let last = compact.pop()?;
        if !(last.is_ascii_alphanumeric() || last == b'+' || last == b'/') {
            return None;
        }
    }
    LENIENT.decode(&compact).ok()
}

/// Collapses runs of HTML whitespace to single spaces; only ASCII whitespace collapses — `&nbsp;` (U+00A0) is meaningful and preserved.
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

fn push_inline(
    inline: &mut Vec<OpenInline>,
    tag: &str,
    start: usize,
    marks: Vec<(&'static str, MarkVal)>,
) {
    inline.push(OpenInline {
        tag: tag.to_string(),
        marks,
        start,
    });
}

/// Closes the nearest open wrapper matching `tag`, emitting its marks over `[start, cur_len)` into `pending`; robust to mis-nesting.
fn close_inline(
    inline: &mut Vec<OpenInline>,
    pending: &mut Vec<PendingMark>,
    tag: &str,
    cur_block: Option<&str>,
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
                pending.push(PendingMark {
                    block: block.to_string(),
                    start: w.start,
                    end: cur_len,
                    name,
                    value,
                });
            }
        }
    }
}

/// Shorthand for `finish_inline` over the current parse state; used at every block boundary.
fn finish_inline_now(st: &mut ImportState) {
    finish_inline(
        &mut st.inline,
        &mut st.pending,
        st.cur_block.as_deref(),
        st.cur_len,
    );
}

/// Closes every still-open inline wrapper, emitting marks up to `cur_len`; called at block boundaries so an unclosed `<b>` doesn't bleed forward.
fn finish_inline(
    inline: &mut Vec<OpenInline>,
    pending: &mut Vec<PendingMark>,
    cur_block: Option<&str>,
    cur_len: usize,
) {
    while let Some(w) = inline.pop() {
        if let Some(block) = cur_block {
            for (name, value) in w.marks {
                pending.push(PendingMark {
                    block: block.to_string(),
                    start: w.start,
                    end: cur_len,
                    name,
                    value,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::Document;
    use tilo_doc::snapshot::DocSnapshot;

    fn import(src: &str) -> DocSnapshot {
        import_with_inputs(src).0
    }

    fn import_with_inputs(src: &str) -> (DocSnapshot, ContentInputs) {
        let mut d = Document::new();
        let mut inputs = ContentInputs::new();
        HtmlCodec
            .import(src.as_bytes(), &mut d.as_mut(), &mut inputs)
            .unwrap();
        (d.materialize(), inputs)
    }

    /// Import `html` as the rich text under `root`, a rich-text field of another document (a
    /// message body): its containers are started when missing and the imported paragraphs appended.
    /// An import holding no paragraph leaves one empty paragraph to type in. Images arrive in
    /// `inputs`.
    fn import_html_under(
        doc: &mut automerge::AutoCommit,
        root: &automerge::ObjId,
        html: &str,
        inputs: &mut ContentInputs,
    ) {
        let c = tilo_doc::model::init(doc, root);
        let mut ids = tilo_doc::model::IdGen::new(doc.get_actor().to_hex_string());
        let mut dm = DocMut {
            doc,
            c: &c,
            ids: &mut ids,
        };
        dm.bulk(|dm| import_html(html, dm, inputs));
        if tilo_doc::model::blocks_in_order(dm.doc, &c).is_empty() {
            dm.insert_block(None, "paragraph");
        }
    }

    #[test]
    fn html_round_trips_through_a_field_of_another_document() {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        let message = doc
            .put_object(automerge::ROOT, "message", automerge::ObjType::Map)
            .unwrap();
        doc.put(&message, "subject", "untouched").unwrap();
        let body = doc
            .put_object(&message, "body", automerge::ObjType::Map)
            .unwrap();
        import_html_under(
            &mut doc,
            &body,
            "<h2>Plan</h2><p><b>ship</b> it</p>",
            &mut ContentInputs::new(),
        );
        let html =
            crate::export_html::render_html_fragment_under(&doc, &body, &tilo_content::NoContent)
                .unwrap();
        assert!(html.contains("<h2>Plan</h2>"), "got: {html}");
        assert!(html.contains("<strong>ship</strong> it"), "got: {html}");
        assert!(
            crate::export_html::render_html_fragment_under(
                &doc,
                &message,
                &tilo_content::NoContent
            )
            .is_none(),
            "the message itself holds no rich text"
        );

        // Nothing to import still leaves a paragraph to type in.
        let empty = doc
            .put_object(&message, "empty", automerge::ObjType::Map)
            .unwrap();
        import_html_under(&mut doc, &empty, "", &mut ContentInputs::new());
        let c = tilo_doc::model::open(&doc, &empty).unwrap();
        assert_eq!(tilo_doc::model::blocks_in_order(&doc, &c).len(), 1);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn html_import_block_structure_and_tolerance() {
        // Phase `headings_and_paragraph`.
        {
            let snap = import("<h1>Title</h1><h2>Sub</h2><p>Body text</p>");
            assert_eq!(snap.blocks[0].kind, "heading");
            assert_eq!(snap.blocks[0].text, "Title");
            assert_eq!(
                snap.blocks[0].attrs.get("level"),
                Some(&tilo_doc::AttrVal::Int(1))
            );
            assert_eq!(snap.blocks[1].kind, "heading");
            assert_eq!(
                snap.blocks[1].attrs.get("level"),
                Some(&tilo_doc::AttrVal::Int(2))
            );
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "Body text");
        }
        // Phase `blockquote_becomes_quote_block`.
        {
            let snap = import("<blockquote><p>quoted line</p></blockquote>");
            let q = snap
                .blocks
                .iter()
                .find(|b| b.kind == "quote")
                .expect("quote block");
            assert!(q.text.contains("quoted line"), "got {:?}", q.text);
        }
        // Phase `table_cells_tagged_table_row_col`.
        {
            let snap =
                import("<table><tr><td>a</td><td>b</td></tr><tr><td>1</td><td>2</td></tr></table>");
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(cells.len(), 4, "{snap:?}");
            assert_eq!(
                cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
                vec!["a", "b", "1", "2"]
            );
            assert_eq!(cells[0].attrs.get("row"), Some(&tilo_doc::AttrVal::Int(0)));
            assert_eq!(cells[0].attrs.get("col"), Some(&tilo_doc::AttrVal::Int(0)));
            assert_eq!(cells[3].attrs.get("row"), Some(&tilo_doc::AttrVal::Int(1)));
            assert_eq!(cells[3].attrs.get("col"), Some(&tilo_doc::AttrVal::Int(1)));
        }
        // Phase `pre_code_is_verbatim_code_block`.
        {
            let snap = import("<pre><code>let x = 1;\nlet y = 2;\n</code></pre>");
            let code = snap
                .blocks
                .iter()
                .find(|b| matches!(b.attrs.get("code"), Some(&tilo_doc::AttrVal::Bool(true))))
                .expect("code block");
            assert_eq!(code.text, "let x = 1;\nlet y = 2;\n");
        }
        // Phase `entities_are_unescaped`.
        {
            let snap = import("<p>a &amp; b &lt; c &gt; d &#65; &#x42; &nbsp;e</p>");
            let p = &snap.blocks[0];
            assert_eq!(p.text, "a & b < c > d A B \u{a0}e");
        }
        // Phase `head_is_skipped`.
        {
            let snap = import("<head><title>Doc Title</title></head><body><p>Body</p></body>");
            let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
            assert_eq!(
                texts,
                vec!["Body".to_string()],
                "title must not leak into body: {snap:?}"
            );
        }
        // Phase `whitespace_is_collapsed`.
        {
            let snap = import("<p>a   b\n  c</p>");
            assert_eq!(snap.blocks[0].text, "a b c");
        }
        // Phase `unclosed_tags_are_tolerated`.
        {
            // No closing `</b>`/`</p>`; the importer must still produce sane blocks.
            let snap = import("<p>start <b>bold and more");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "start bold and more");
            assert!(
                p.marks.iter().any(|m| m.name == "strong"),
                "unclosed bold still marks: {:?}",
                p.marks
            );
        }
        // Phase `export_is_unsupported`.
        {
            let d = Document::new();
            let err = HtmlCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap_err();
            assert!(matches!(err, CodecError::Unsupported(_)), "got {err:?}");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn html_import_marks_and_styles() {
        // Phase `bold_italic_underline_strike_marks`.
        {
            let snap = import("<p>a<strong>b</strong><em>c</em><u>d</u><s>e</s></p>");
            let p = &snap.blocks[0];
            assert_eq!(p.text, "abcde");
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 1 && m.end == 2),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "em" && m.start == 2 && m.end == 3),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "underline" && m.start == 3 && m.end == 4),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strike" && m.start == 4 && m.end == 5),
                "{:?}",
                p.marks
            );
        }
        // Phase `b_and_i_aliases_map_to_strong_em`.
        {
            let snap = import("<p><b>x</b> <i>y</i></p>");
            let p = &snap.blocks[0];
            assert_eq!(p.text, "x y");
            assert!(p.marks.iter().any(|m| m.name == "strong"), "{:?}", p.marks);
            assert!(p.marks.iter().any(|m| m.name == "em"), "{:?}", p.marks);
        }
        // Phase `link_becomes_mark`.
        {
            let snap = import(r#"<p>See <a href="https://tilo.so/">the docs</a> now</p>"#);
            let p = &snap.blocks[0];
            assert_eq!(p.text, "See the docs now");
            let link = p
                .marks
                .iter()
                .find(|m| m.name == "link")
                .expect("link mark");
            assert_eq!(
                link.value,
                tilo_doc::document::MarkValue::Str("https://tilo.so/".into())
            );
            assert_eq!(p.text.get(link.start..link.end), Some("the docs"));
        }
        // Phase `span_style_carries_color_size_font`.
        {
            let snap = import(
                r#"<p><span style="color: #ff0000; font-size: 20pt; font-family: serif">styled</span></p>"#,
            );
            let p = &snap.blocks[0];
            assert_eq!(p.text, "styled");
            assert!(
                p.marks.iter().any(|m| m.name == "color"
                    && m.value == tilo_doc::document::MarkValue::Str("#ff0000".into())),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "fontSize"
                        && m.value == tilo_doc::document::MarkValue::Int(20)),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks.iter().any(|m| m.name == "fontFamily"
                    && m.value == tilo_doc::document::MarkValue::Str("serif".into())),
                "{:?}",
                p.marks
            );
        }
        // Phase `background_becomes_highlight`.
        {
            let snap = import(r#"<p><span style="background: #ffff00">hi</span></p>"#);
            let p = &snap.blocks[0];
            assert!(
                p.marks.iter().any(|m| m.name == "highlight"
                    && m.value == tilo_doc::document::MarkValue::Str("#ffff00".into())),
                "{:?}",
                p.marks
            );
        }
        // Phase `sup_and_sub_marks`.
        {
            let snap = import("<p>x<sup>2</sup>y<sub>n</sub></p>");
            let p = &snap.blocks[0];
            assert_eq!(p.text, "x2yn");
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "super" && m.start == 1 && m.end == 2),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "sub" && m.start == 3 && m.end == 4),
                "{:?}",
                p.marks
            );
        }
    }

    #[test]
    fn html_import_lists_carry_kind_and_level() {
        // Phase `unordered_and_ordered_list_items`.
        {
            let snap = import("<ul><li>one</li><li>two</li></ul><ol><li>first</li></ol>");
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 3, "{snap:?}");
            assert_eq!(items[0].text, "one");
            assert_eq!(
                items[0].attrs.get("ordered"),
                Some(&tilo_doc::AttrVal::Bool(false))
            );
            assert_eq!(
                items[2].attrs.get("ordered"),
                Some(&tilo_doc::AttrVal::Bool(true))
            );
        }
        // Phase `nested_lists_carry_level`.
        {
            let snap = import("<ul><li>top</li><ul><li>sub</li></ul></ul>");
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 2);
            let sub = items.iter().find(|b| b.text == "sub").expect("sub item");
            assert_eq!(
                sub.attrs.get("level"),
                Some(&tilo_doc::AttrVal::Int(1)),
                "{:?}",
                sub.attrs
            );
        }
    }

    #[test]
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "the embedded CSS `p{color:red}` is opaque test fixture text, not a format string"
    )]
    fn script_and_style_are_skipped() {
        let snap = import(
            "<p>before</p><script>alert('x'); 1 < 2;</script><style>p{color:red}</style><p>after</p>",
        );
        let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
        assert_eq!(
            texts,
            vec!["before".to_string(), "after".to_string()],
            "{snap:?}"
        );
    }

    #[test]
    fn html_import_images_decode_data_uris_and_keep_remote_src() {
        // Phase `base64_payload_decoding_is_lenient_about_layout_only`.
        {
            // Padded, unpadded and wrapped payloads decode alike.
            assert_eq!(base64_decode("Zm9vYg==").as_deref(), Some(&b"foob"[..]));
            assert_eq!(base64_decode("Zm9vYg").as_deref(), Some(&b"foob"[..]));
            assert_eq!(
                base64_decode(" Zm9v\r\n YmFy\t").as_deref(),
                Some(&b"foobar"[..])
            );
            assert_eq!(
                base64_decode("+/+/").as_deref(),
                Some(&[0xfb, 0xff, 0xbf][..])
            );
            assert_eq!(base64_decode("").as_deref(), Some(&[][..]));
            assert_eq!(base64_decode(" \n").as_deref(), Some(&[][..]));
            // Decoding stops at the first `=`; whatever follows is ignored.
            assert_eq!(base64_decode("QQ==garbage!").as_deref(), Some(&b"A"[..]));
            assert_eq!(base64_decode("QQ=").as_deref(), Some(&b"A"[..]));
            // Non-zero trailing bits and a dangling final sextet are dropped.
            assert_eq!(base64_decode("QR==").as_deref(), Some(&b"A"[..]));
            assert_eq!(base64_decode("Zm9vY").as_deref(), Some(&b"foo"[..]));
            assert_eq!(base64_decode("Q").as_deref(), Some(&[][..]));
            // Anything outside the standard alphabet is rejected, even last.
            assert_eq!(base64_decode("Zm9v-_"), None);
            assert_eq!(base64_decode("Zm9vY!"), None);
            assert_eq!(base64_decode("!"), None);
            assert_eq!(base64_decode("Zm9v\u{a0}"), None);
        }
        // Phase `data_uri_image_becomes_blob`.
        {
            // A 1-byte payload "A" (base64 "QQ==").
            let (snap, inputs) =
                import_with_inputs(r#"<p><img src="data:image/png;base64,QQ==" alt="pic"></p>"#);
            let img = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .expect("image block");
            assert_eq!(img.text, "pic");
            let blob = match img.attrs.get("blob") {
                Some(tilo_doc::AttrVal::Str(s)) => s.clone(),
                other => panic!("expected blob attr, got {other:?}"),
            };
            assert_eq!(inputs.get(&blob), Some(b"A".as_slice()), "{inputs:?}");
        }
        // Phase `http_image_keeps_src`.
        {
            let snap = import(r#"<img src="https://example.com/p.png" alt="remote">"#);
            let img = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .expect("image block");
            assert_eq!(img.text, "remote");
            assert_eq!(
                img.attrs.get("src"),
                Some(&tilo_doc::AttrVal::Str("https://example.com/p.png".into()))
            );
        }
    }
}
